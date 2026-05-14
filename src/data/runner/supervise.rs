//! Supervises managed ACP runs: builds launch contexts, drives the per-run
//! polling adapter on the App-owned tokio runtime, owns the keyed
//! run registry, and exposes the public control surface the orchestrator uses
//! to start, interrupt, and tear down agent runs.
//!
//! Transport-side helpers (channels, text accumulation, ACP traces) live in
//! [`super::transport`]; finish-stamp/git/exit-policy primitives live in
//! [`super::exit`]. This file is the supervisor that ties them together.
mod launch;
mod runtime;
#[cfg(test)]
#[path = "supervise/tests_support.rs"]
mod test_support;
use super::transport::{AcpCancelReason, AcpInput, ManagedAcpLaunch};
use crate::data::acp::AcpLaunchPolicy;
use crate::data::adapters::AgentRun;
use anyhow::{Result, bail};
use dashmap::DashMap;
pub(in crate::data::runner) use launch::append_launch_cause;
use launch::build_managed_acp_launch;
use parking_lot::Mutex as PlMutex;
use runtime::finalize_managed_acp_launch;
use std::{path::Path, sync::Arc};
#[cfg(test)]
pub(in crate::data::runner) use test_support::{assign_test_run_id, test_supervisor};
#[cfg(test)]
pub use test_support::{
    cancel_run_labels_matching, drain_test_cancel_receiver_for, drain_test_input_receiver_for,
    force_interrupt_run_label, interrupt_run_label_input, register_test_run_id,
    request_run_label_active_for_test, request_run_label_exit,
    request_run_label_interactive_input_for_test, run_label_is_active,
    run_label_is_waiting_for_input, send_run_label_input, shutdown_all_runs, terminate_run_label,
};
use tokio::{
    runtime::Handle,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
#[derive(Debug, Clone)]
pub(super) struct ManagedAcpOutcome {
    exit_code: i32,
    signal_received: String,
}
/// Cancel coordination for a single managed run. The token wakes the run
/// promptly via `pending_reason()`; the parking-lot slot conveys whether the
/// wake should result in `Terminate` (kill) or `Complete` (graceful close +
/// artifact validate). One signal is observable; subsequent calls are no-ops
/// so a `Complete` cannot be silently downgraded by a stray late `Terminate`.
pub type RunId = u64;
#[derive(Debug)]
struct CancelSignal {
    token: CancellationToken,
    reason: PlMutex<Option<AcpCancelReason>>,
}
impl CancelSignal {
    fn new(token: CancellationToken) -> Self {
        Self {
            token,
            reason: PlMutex::new(None),
        }
    }
    fn signal(&self, reason: AcpCancelReason) {
        let mut slot = self.reason.lock();
        if slot.is_none() {
            *slot = Some(reason);
        }
        self.token.cancel();
    }
    fn pending_reason(&self) -> Option<AcpCancelReason> {
        if !self.token.is_cancelled() {
            return None;
        }
        Some(
            self.reason
                .lock()
                .take()
                .unwrap_or(AcpCancelReason::Terminate),
        )
    }
}
/// Per-run state stored in the `Supervisor` registry. The cancel signal,
/// input sender, and watch receivers form the explicit async control surface
/// the App and the inner run task share. The join handle is `None` for
/// fixture-only entries registered by `request_run_label_*_for_test`.
#[derive(Debug)]
struct RunHandle {
    #[cfg(test)]
    window_name: String,
    cancel: Arc<CancelSignal>,
    input_tx: mpsc::UnboundedSender<AcpInput>,
    waiting_for_input: watch::Receiver<bool>,
    finished: watch::Receiver<bool>,
    join: Option<JoinHandle<()>>,
}
impl RunHandle {
    fn is_finished(&self) -> bool {
        *self.finished.borrow()
    }
    fn is_waiting_for_input(&self) -> bool {
        *self.waiting_for_input.borrow()
    }
}
/// Process-supervision root owned by `App`. The registry is keyed by the
/// persisted run id so cancellation/input/status target the durable run
/// identity rather than the human-readable window label.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<SupervisorInner>,
}
struct SupervisorInner {
    config: Arc<crate::data::config::Config>,
    handle: Option<Handle>,
    root_token: CancellationToken,
    runs: DashMap<RunId, RunHandle>,
}
impl Default for Supervisor {
    fn default() -> Self {
        Self::new(Arc::new(crate::data::config::Config::baked_defaults()))
    }
}
impl Supervisor {
    pub fn new(config: Arc<crate::data::config::Config>) -> Self {
        let handle = Handle::try_current().ok();
        #[cfg(test)]
        let handle = handle.or_else(test_runtime_handle);
        Self {
            inner: Arc::new(SupervisorInner {
                config,
                handle,
                root_token: CancellationToken::new(),
                runs: DashMap::new(),
            }),
        }
    }
    #[cfg(test)]
    pub fn shared_for_test() -> Self {
        test_supervisor().clone()
    }
    /// Drain RunHandles whose inner task has signalled `finished=true`,
    /// joining their tasks so resources release deterministically before the
    /// next public-control-surface call inspects the map.
    fn cleanup_finished(&self) {
        let finished_keys: Vec<RunId> = self
            .inner
            .runs
            .iter()
            .filter(|entry| entry.value().is_finished())
            .map(|entry| *entry.key())
            .collect();
        for run_id in finished_keys {
            self.remove_and_join(run_id);
        }
    }
    pub fn run_is_active(&self, run_id: RunId) -> bool {
        self.inner
            .runs
            .get(&run_id)
            .is_some_and(|run| !run.is_finished())
    }
    pub fn run_is_waiting_for_input(&self, run_id: RunId) -> bool {
        self.cleanup_finished();
        self.inner
            .runs
            .get(&run_id)
            .is_some_and(|run| !run.is_finished() && run.is_waiting_for_input())
    }
    fn any_run_unfinished(&self) -> bool {
        self.inner
            .runs
            .iter()
            .any(|entry| !entry.value().is_finished())
    }
    fn child_cancel_signal(&self) -> Arc<CancelSignal> {
        Arc::new(CancelSignal::new(self.inner.root_token.child_token()))
    }
    fn spawn<F>(&self, task: F) -> Result<JoinHandle<()>>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let Some(handle) = &self.inner.handle else {
            bail!("runner supervisor requires an active tokio runtime");
        };
        Ok(handle.spawn(task))
    }
    fn join(&self, join: JoinHandle<()>) {
        let Some(handle) = &self.inner.handle else {
            return;
        };
        if Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| {
                let _ = handle.block_on(join);
            });
        } else {
            let _ = handle.block_on(join);
        }
    }
    fn remove_and_join(&self, run_id: RunId) -> Option<RunHandle> {
        let removed = self.inner.runs.remove(&run_id).map(|(_, run)| run);
        if let Some(mut run) = removed {
            if let Some(join) = run.join.take() {
                self.join(join);
            }
            Some(run)
        } else {
            None
        }
    }
    fn take_run(&self, run_id: RunId) -> Option<RunHandle> {
        self.inner.runs.remove(&run_id).map(|(_, run)| run)
    }
    #[cfg(test)]
    fn matching_label_run_ids(&self, base: &str) -> Vec<RunId> {
        let prefix = format!("{base} ");
        self.inner
            .runs
            .iter()
            .filter(|entry| {
                let name = &entry.value().window_name;
                name == base || name.starts_with(&prefix)
            })
            .map(|entry| *entry.key())
            .collect()
    }
}
#[cfg(test)]
fn test_runtime_handle() -> Option<Handle> {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    Some(
        RUNTIME
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("codexize-runner-test")
                    .build()
                    .expect("failed to build runner test runtime")
            })
            .handle()
            .clone(),
    )
}
fn launch_managed_acp_window(
    supervisor: &Supervisor,
    run_id: RunId,
    _window_name: &str,
    launch: ManagedAcpLaunch,
) -> Result<()> {
    supervisor.cleanup_finished();
    if supervisor.any_run_unfinished() {
        bail!("codexize only supports one active ACP run at a time");
    }
    let cancel = supervisor.child_cancel_signal();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
    let (waiting_tx, waiting_rx) = watch::channel(false);
    let (finished_tx, finished_rx) = watch::channel(false);
    let cancel_for_task = Arc::clone(&cancel);
    let task = async move {
        let span = tracing::debug_span!(
            "agent_run",
            run_id,
            window_name = %launch.window_name,
            interactive = launch.resolved.interactive,
            cli = ?launch.resolved.cli
        );
        async move {
            tracing::debug!("managed ACP run started");
            finalize_managed_acp_launch(launch, cancel_for_task, input_rx, waiting_tx).await;
            tracing::debug!("managed ACP run finished");
            let _ = finished_tx.send(true);
        }
        .instrument(span)
        .await;
    };
    let join = supervisor.spawn(task)?;
    supervisor.inner.runs.insert(
        run_id,
        RunHandle {
            #[cfg(test)]
            window_name: _window_name.to_string(),
            cancel,
            input_tx,
            waiting_for_input: waiting_rx,
            finished: finished_rx,
            join: Some(join),
        },
    );
    Ok(())
}
impl Supervisor {
    #[cfg(test)]
    pub fn cancel_runs_matching_label(&self, base: &str) {
        for run_id in self.matching_label_run_ids(base) {
            if let Some(mut run) = self.take_run(run_id) {
                run.cancel.signal(AcpCancelReason::Terminate);
                if let Some(join) = run.join.take() {
                    self.join(join);
                }
            }
        }
    }
    pub fn request_run_exit(&self, run_id: RunId) {
        if let Some(mut run) = self.take_run(run_id) {
            run.cancel.signal(AcpCancelReason::Complete);
            if let Some(join) = run.join.take() {
                self.join(join);
            }
        }
    }
    pub fn cancel_run(&self, run_id: RunId) {
        if let Some(mut run) = self.take_run(run_id) {
            run.cancel.signal(AcpCancelReason::Terminate);
            if let Some(join) = run.join.take() {
                self.join(join);
            }
        }
    }
    pub fn send_run_input(&self, run_id: RunId, text: String) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        self.cleanup_finished();
        self.inner
            .runs
            .get(&run_id)
            .filter(|run| !run.is_finished() && run.is_waiting_for_input())
            .is_some_and(|run| run.input_tx.send(AcpInput::Prompt(text)).is_ok())
    }
    pub fn interrupt_run_input(&self, run_id: RunId, text: String) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        self.cleanup_finished();
        self.inner.runs.get(&run_id).is_some_and(|run| {
            if run.is_finished() {
                return false;
            }
            let input = if run.is_waiting_for_input() {
                AcpInput::Prompt(text)
            } else {
                AcpInput::Interrupt(text)
            };
            run.input_tx.send(input).is_ok()
        })
    }
    /// Push an `AcpInput::Interrupt(text)` onto the run's input channel
    /// regardless of whether the runner reports it is waiting for input. Used
    /// by the watchdog warning path where cancelling the in-flight ACP turn is
    /// required before queueing the warning as the next prompt.
    pub fn force_interrupt_run(&self, run_id: RunId, text: String) -> bool {
        if text.is_empty() {
            return false;
        }
        self.cleanup_finished();
        self.inner.runs.get(&run_id).is_some_and(|run| {
            if run.is_finished() {
                return false;
            }
            run.input_tx.send(AcpInput::Interrupt(text)).is_ok()
        })
    }
    /// Best-effort `AcpCancelReason::Terminate` for the run id (spec §3.5).
    /// The handle stays registered until the task exits so `poll_agent_run`
    /// observes the normal transition from active to finished and routes the
    /// non-zero exit through existing vendor failover.
    pub fn terminate_run(&self, run_id: RunId) -> bool {
        self.cleanup_finished();
        self.inner.runs.get(&run_id).is_some_and(|run| {
            if run.is_finished() {
                return false;
            }
            run.cancel.signal(AcpCancelReason::Terminate);
            true
        })
    }
    pub fn shutdown_all_runs(&self) {
        let runs: Vec<(RunId, RunHandle)> = {
            let keys: Vec<RunId> = self.inner.runs.iter().map(|entry| *entry.key()).collect();
            keys.into_iter()
                .filter_map(|key| self.inner.runs.remove(&key))
                .collect()
        };
        #[cfg(test)]
        {
            test_support::clear_fixture_state();
        }
        for (_, mut run) in runs {
            run.cancel.signal(AcpCancelReason::Terminate);
            if let Some(join) = run.join.take() {
                self.join(join);
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn launch_managed(
        &self,
        run_id: RunId,
        window_name: &str,
        run: &AgentRun,
        run_key: &str,
        artifacts_dir: &Path,
        required_artifact: Option<&Path>,
        interactive: bool,
        policy: AcpLaunchPolicy,
    ) -> Result<()> {
        let acp_config = crate::data::acp::AcpConfig::from_config_views(
            &self.inner.config.acp.agents,
            &self.inner.config.acp_install_view(),
        );
        let launch = build_managed_acp_launch(
            &acp_config,
            window_name,
            run,
            run_key,
            artifacts_dir,
            required_artifact,
            interactive,
            policy,
        )?;
        launch_managed_acp_window(self, run_id, window_name, launch)
    }
}
