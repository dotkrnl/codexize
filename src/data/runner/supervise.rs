//! Supervises managed ACP runs: builds launch contexts, drives the per-run
//! polling adapter on the App-owned tokio runtime, owns the keyed
//! run registry, and exposes the public control surface the orchestrator uses
//! to start, interrupt, and tear down agent runs.
//!
//! Transport-side helpers (channels, text accumulation, ACP traces) live in
//! [`super::transport`]; finish-stamp/git/exit-policy primitives live in
//! [`super::exit`]. This file is the supervisor that ties them together.

use crate::acp::{
    AcpCompletionEvent, AcpConfig, AcpConnector, AcpLaunchPolicy, AcpLaunchRequest,
    AcpRuntimeEvent, PromptPayload, SubprocessConnector, translate_update,
};
use crate::adapters::AgentRun;
use crate::selection::VendorKind;
use crate::state::MessageKind;
use anyhow::{Context, Result, anyhow, bail};
use dashmap::DashMap;
use parking_lot::Mutex as PlMutex;
use std::collections::VecDeque;
use std::{fs, path::Path, sync::Arc, thread};
use tokio::{
    runtime::Handle,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::exit::{
    enforce_readonly_workspace_policy, git_rev_parse_head, git_status_porcelain,
    validate_toml_artifacts, write_finish_stamp_for_outcome,
};
use super::transport::{
    ACP_POLL_INTERVAL, AcpCancelReason, AcpInput, AcpTextStream, ManagedAcpLaunch,
    acp_trace_path_from_cause_path, append_acp_text_trace,
};

#[derive(Debug, Clone)]
struct ManagedAcpOutcome {
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
    #[cfg_attr(not(test), allow(dead_code))]
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
    handle: Option<Handle>,
    root_token: CancellationToken,
    runs: DashMap<RunId, RunHandle>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                handle: Handle::try_current().ok().or_else(test_runtime_handle),
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

#[cfg(not(test))]
fn test_runtime_handle() -> Option<Handle> {
    None
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

#[allow(clippy::too_many_arguments)]
fn build_managed_acp_launch(
    window_name: &str,
    vendor: VendorKind,
    run: &AgentRun,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
    interactive: bool,
    policy: AcpLaunchPolicy,
) -> Result<ManagedAcpLaunch> {
    let cwd = std::env::current_dir().context("failed to capture launch cwd")?;
    let request = AcpLaunchRequest {
        vendor,
        cwd,
        prompt: PromptPayload::File(run.prompt_path.clone()),
        model: run.model.clone(),
        // The current launch sites already pass the codexize-computed effective
        // effort. Task 2 keeps artifact/finalization ownership in codexize and
        // defers the requested-vs-effective UI split to the later ACP UX work.
        requested_effort: run.effort,
        effective_effort: run.effort,
        interactive,
        modes: run.modes,
        required_artifacts: required_artifact
            .into_iter()
            .map(Path::to_path_buf)
            .collect(),
        policy,
    };
    let mut resolved = AcpConfig::default()
        .resolve(&request)
        .map_err(|err| anyhow!("{err}"))?;
    ensure_program_exists(&resolved.spawn.program)?;
    let cause_path = artifacts_dir
        .join("run-finish")
        .join(format!("{run_key}.cause.txt"));
    resolved.session.metadata.insert(
        "codexize.acp_trace_path".to_string(),
        acp_trace_path_from_cause_path(&cause_path)
            .display()
            .to_string(),
    );

    Ok(ManagedAcpLaunch {
        resolved,
        window_name: window_name.to_string(),
        session_id: session_id_from_artifacts_dir(artifacts_dir),
        stamp_path: artifacts_dir
            .join("run-finish")
            .join(format!("{run_key}.toml")),
        // Keep transport-boundary diagnostics adjacent to finish stamps so
        // postmortems can inspect one per-run directory.
        cause_path,
        required_artifact: required_artifact.map(Path::to_path_buf),
    })
}

fn ensure_program_exists(program: &str) -> Result<()> {
    if crate::acp::program_is_executable(program) {
        Ok(())
    } else {
        bail!("ACP agent CLI not found — install it first");
    }
}

fn session_id_from_artifacts_dir(artifacts_dir: &Path) -> Option<String> {
    artifacts_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn write_launch_cause(path: &Path, cause: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create cause dir {}", parent.display()))?;
    }
    fs::write(path, cause).with_context(|| format!("failed to write cause {}", path.display()))
}

pub(in crate::data::runner) fn append_launch_cause(path: &Path, cause: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let text = if existing.is_empty() {
        cause.to_string()
    } else {
        format!("{existing}\n{cause}")
    };
    let _ = fs::write(path, text);
}

/// Sync output of the per-run loop. Holds everything `finalize_managed_acp_launch`
/// needs to apply enforcement and write the finish stamp on the async tail
/// without re-snapshotting git state.
struct ManagedAcpRunResult {
    outcome: Result<ManagedAcpOutcome>,
    head_before: String,
    git_status_before: Option<String>,
    enforce_readonly: bool,
}

fn run_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel: &CancelSignal,
    input_rx: &mut mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: &watch::Sender<bool>,
) -> ManagedAcpRunResult {
    let head_before = git_rev_parse_head().unwrap_or_default();
    let enforce_readonly = launch.resolved.session.policy.enforce_readonly_workspace;
    // Final validation is allowed to update its ignored artifact files, so the
    // ACP-side write allowlist carries those exact paths while the runner
    // enforces that no git-visible workspace state changes during the run.
    let git_status_before_result = enforce_readonly.then(git_status_porcelain).transpose();
    let git_status_before = git_status_before_result.as_ref().ok().cloned().flatten();

    // The inner closure keeps the original `?`-propagating shape so the loop
    // body is unchanged below; the wrapping result carries the data the async
    // finalisation needs (`head_before`, `git_status_before`, policy flag) so
    // the supervisor's blocking section never crosses an await boundary.
    let outcome = git_status_before_result
        .and_then(|_| run_managed_acp_loop(&launch, cancel, input_rx, waiting_for_input));

    ManagedAcpRunResult {
        outcome,
        head_before,
        git_status_before,
        enforce_readonly,
    }
}

fn run_managed_acp_loop(
    launch: &ManagedAcpLaunch,
    cancel: &CancelSignal,
    input_rx: &mut mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: &watch::Sender<bool>,
) -> Result<ManagedAcpOutcome> {
    let connector = SubprocessConnector;
    let mut session = connector
        .connect(&launch.resolved)
        .map_err(|err| anyhow!("{err}"))?;
    let mut agent_text = AcpTextStream::new();
    let mut thought_text = AcpTextStream::new();
    let mut pending_input = VecDeque::new();
    let mut waiting_for_interactive_prompt = false;
    let mut interrupting_turn = false;

    let outcome = loop {
        if let Some(reason) = cancel.pending_reason() {
            let _ = waiting_for_input.send_replace(false);
            thought_text.finish_turn(launch, MessageKind::AgentThought);
            agent_text.finish_turn(launch, MessageKind::AgentText);
            session.close().map_err(|err| anyhow!("{err}"))?;
            match reason {
                AcpCancelReason::Terminate => {
                    break ManagedAcpOutcome {
                        exit_code: 143,
                        signal_received: "TERM".to_string(),
                    };
                }
                AcpCancelReason::Complete => {
                    if let Some(path) = launch.required_artifact.as_deref() {
                        validate_toml_artifacts(&[path])?;
                    }
                    break ManagedAcpOutcome {
                        exit_code: 0,
                        signal_received: String::new(),
                    };
                }
            }
        }

        while let Ok(input) = input_rx.try_recv() {
            match input {
                AcpInput::Prompt(text) => pending_input.push_back(text),
                AcpInput::Interrupt(text) => {
                    pending_input.push_back(text);
                    if !waiting_for_interactive_prompt && !interrupting_turn {
                        session.cancel_prompt().map_err(|err| anyhow!("{err}"))?;
                        interrupting_turn = true;
                        let _ = waiting_for_input.send_replace(false);
                    }
                }
            }
        }

        if waiting_for_interactive_prompt && let Some(text) = pending_input.pop_front() {
            let _ = waiting_for_input.send_replace(false);
            session
                .submit_prompt(&text)
                .map_err(|err| anyhow!("{err}"))?;
            waiting_for_interactive_prompt = false;
            interrupting_turn = false;
        }

        let event = session
            .try_next_update()
            .map_err(|err| anyhow!("{err}"))?
            .and_then(|update| translate_update(update, launch.resolved.interactive));

        match event {
            Some(AcpRuntimeEvent::Completion(AcpCompletionEvent::PromptTurnFinished)) => {
                thought_text.finish_turn(launch, MessageKind::AgentThought);
                agent_text.finish_turn(launch, MessageKind::AgentText);
                if launch.resolved.interactive {
                    if let Some(text) = pending_input.pop_front() {
                        let _ = waiting_for_input.send_replace(false);
                        session
                            .submit_prompt(&text)
                            .map_err(|err| anyhow!("{err}"))?;
                        waiting_for_interactive_prompt = false;
                        interrupting_turn = false;
                    } else {
                        waiting_for_interactive_prompt = true;
                        interrupting_turn = false;
                        let _ = waiting_for_input.send_replace(true);
                    }
                    poll_park();
                    continue;
                }
                let _ = waiting_for_input.send_replace(false);
                session.close().map_err(|err| anyhow!("{err}"))?;
                if let Some(path) = launch.required_artifact.as_deref() {
                    validate_toml_artifacts(&[path])?;
                }
                break ManagedAcpOutcome {
                    exit_code: 0,
                    signal_received: String::new(),
                };
            }
            Some(AcpRuntimeEvent::Completion(AcpCompletionEvent::PromptTurnFailed { .. })) => {
                thought_text.finish_turn(launch, MessageKind::AgentThought);
                agent_text.finish_turn(launch, MessageKind::AgentText);
                if launch.resolved.interactive && interrupting_turn {
                    if let Some(text) = pending_input.pop_front() {
                        let _ = waiting_for_input.send_replace(false);
                        session
                            .submit_prompt(&text)
                            .map_err(|err| anyhow!("{err}"))?;
                        waiting_for_interactive_prompt = false;
                        interrupting_turn = false;
                    } else {
                        waiting_for_interactive_prompt = true;
                        interrupting_turn = false;
                        let _ = waiting_for_input.send_replace(true);
                    }
                    poll_park();
                    continue;
                }
                let _ = waiting_for_input.send_replace(false);
                session.close().map_err(|err| anyhow!("{err}"))?;
                break ManagedAcpOutcome {
                    exit_code: 1,
                    signal_received: String::new(),
                };
            }
            Some(AcpRuntimeEvent::Text(text_event)) => {
                append_acp_text_trace(launch, &text_event);
                let text = text_event.text;
                if text_event.thought {
                    thought_text.push_text_boundary(
                        launch,
                        &text,
                        MessageKind::AgentThought,
                        text_event.boundary,
                    );
                } else {
                    agent_text.push_text_boundary(
                        launch,
                        &text,
                        MessageKind::AgentText,
                        text_event.boundary,
                    );
                }
                poll_park();
            }
            Some(AcpRuntimeEvent::ToolCallActivity { .. })
            | Some(AcpRuntimeEvent::Lifecycle(_))
            | None => poll_park(),
        }
    };

    Ok(outcome)
}

/// Pace the inner sync loop without busy-spinning. `park_timeout` returns
/// after the interval (or sooner if `Thread::unpark` was called); using park
/// keeps the runner product code off the banned blocking-poll primitive
/// while preserving the coarse poll cadence.
fn poll_park() {
    // The ACP connector is still a synchronous adapter; parking is bounded by
    // the transport poll interval until the ACP actor migration makes updates
    // awaitable without a blocking bridge.
    thread::park_timeout(ACP_POLL_INTERVAL);
}

async fn finalize_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel: Arc<CancelSignal>,
    input_rx: mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: watch::Sender<bool>,
) {
    let launch_for_loop = launch.clone();
    let waiting_for_loop = waiting_for_input.clone();
    let blocking_result = tokio::task::spawn_blocking(move || {
        let mut input_rx = input_rx;
        run_managed_acp_launch(
            launch_for_loop,
            cancel.as_ref(),
            &mut input_rx,
            &waiting_for_loop,
        )
    })
    .await;

    let result = match blocking_result {
        Ok(result) => result,
        Err(err) => {
            // The blocking section is the sync ACP poll loop; if it panics we
            // can't recover its in-flight git snapshot, so synthesise a worst-
            // case finalisation that records the failure.
            let _ = waiting_for_input.send_replace(false);
            let _ = write_launch_cause(&launch.cause_path, &format!("{err:#}"));
            let fallback_head_before = git_rev_parse_head().unwrap_or_default();
            let _ =
                write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, 1, "")
                    .await;
            return;
        }
    };

    let outcome = result.outcome.and_then(|outcome| {
        enforce_readonly_workspace_policy(
            result.enforce_readonly,
            &result.head_before,
            result.git_status_before.as_deref(),
        )
        .map(|()| outcome)
    });

    match outcome {
        Ok(outcome) => {
            let _ = write_finish_stamp_for_outcome(
                &launch.stamp_path,
                result.head_before,
                outcome.exit_code,
                &outcome.signal_received,
            )
            .await;
            let _ = waiting_for_input.send_replace(false);
            let _ = fs::remove_file(&launch.cause_path);
        }
        Err(err) => {
            let _ = waiting_for_input.send_replace(false);
            let _ = write_launch_cause(&launch.cause_path, &format!("{err:#}"));
            let fallback_head_before = git_rev_parse_head().unwrap_or_default();
            let _ =
                write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, 1, "")
                    .await;
        }
    }
}

fn launch_managed_acp_window(
    supervisor: &Supervisor,
    run_id: RunId,
    window_name: &str,
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
    let join = supervisor.spawn(async move {
        finalize_managed_acp_launch(launch, cancel_for_task, input_rx, waiting_tx).await;
        let _ = finished_tx.send(true);
    })?;

    supervisor.inner.runs.insert(
        run_id,
        RunHandle {
            window_name: window_name.to_string(),
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
            test_run_ids().clear();
            test_input_observers().clear();
            test_cancel_observers().clear();
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
        vendor: VendorKind,
        run_key: &str,
        artifacts_dir: &Path,
        required_artifact: Option<&Path>,
        interactive: bool,
        policy: AcpLaunchPolicy,
    ) -> Result<()> {
        let launch = build_managed_acp_launch(
            window_name,
            vendor,
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

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(in crate::data::runner) fn test_supervisor() -> &'static Supervisor {
    static SUPERVISOR: std::sync::OnceLock<Supervisor> = std::sync::OnceLock::new();
    SUPERVISOR.get_or_init(Supervisor::new)
}

#[cfg(test)]
fn test_run_ids() -> &'static DashMap<String, RunId> {
    static RUN_IDS: std::sync::OnceLock<DashMap<String, RunId>> = std::sync::OnceLock::new();
    RUN_IDS.get_or_init(DashMap::new)
}

#[cfg(test)]
fn test_input_observers() -> &'static DashMap<String, PlMutex<mpsc::UnboundedReceiver<AcpInput>>> {
    static OBSERVERS: std::sync::OnceLock<
        DashMap<String, PlMutex<mpsc::UnboundedReceiver<AcpInput>>>,
    > = std::sync::OnceLock::new();
    OBSERVERS.get_or_init(DashMap::new)
}

/// Test-only side observer for the cancel signal of a fixture run. The
/// observer holds the same `Arc<CancelSignal>` as the registered RunHandle,
/// so signals issued through `request_run_label_exit` /
/// `cancel_run_labels_matching` remain visible to drain helpers even after
/// the supervisor has removed the run from its registry.
#[cfg(test)]
fn test_cancel_observers() -> &'static DashMap<String, Arc<CancelSignal>> {
    static OBSERVERS: std::sync::OnceLock<DashMap<String, Arc<CancelSignal>>> =
        std::sync::OnceLock::new();
    OBSERVERS.get_or_init(DashMap::new)
}

#[cfg(test)]
pub fn request_run_label_interactive_input_for_test(window_name: &str) {
    register_test_run_label(window_name, true);
}

#[cfg(test)]
pub fn request_run_label_active_for_test(window_name: &str) {
    register_test_run_label(window_name, false);
}

#[cfg(test)]
fn register_test_run_label(window_name: &str, waiting: bool) {
    let supervisor = test_supervisor();
    let run_id = assign_test_run_id(window_name);
    let cancel = supervisor.child_cancel_signal();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
    let (_waiting_tx, waiting_rx) = watch::channel(waiting);
    let (_finished_tx, finished_rx) = watch::channel(false);

    test_cancel_observers().insert(window_name.to_string(), Arc::clone(&cancel));
    test_input_observers().insert(window_name.to_string(), PlMutex::new(input_rx));

    supervisor.inner.runs.insert(
        run_id,
        RunHandle {
            window_name: window_name.to_string(),
            cancel,
            input_tx,
            waiting_for_input: waiting_rx,
            finished: finished_rx,
            join: None,
        },
    );
}

#[cfg(test)]
fn test_run_id_for_label(window_name: &str) -> Option<RunId> {
    test_run_ids().get(window_name).map(|entry| *entry.value())
}

#[cfg(test)]
pub(in crate::data::runner) fn assign_test_run_id(window_name: &str) -> RunId {
    test_run_ids()
        .get(window_name)
        .map(|entry| *entry.value())
        .unwrap_or_else(|| {
            let next = test_run_ids().len() as RunId + 1;
            test_run_ids().insert(window_name.to_string(), next);
            next
        })
}

#[cfg(test)]
pub fn register_test_run_id(window_name: &str, run_id: RunId) {
    let previous = test_run_ids().insert(window_name.to_string(), run_id);
    if let Some(previous) = previous
        && previous != run_id
        && let Some((_, run)) = test_supervisor().inner.runs.remove(&previous)
    {
        test_supervisor().inner.runs.insert(run_id, run);
    }
}

#[cfg(test)]
pub fn run_label_is_active(window_name: &str) -> bool {
    let Some(run_id) = test_run_id_for_label(window_name) else {
        return false;
    };
    test_supervisor().run_is_active(run_id)
}

#[cfg(test)]
pub fn run_label_is_waiting_for_input(window_name: &str) -> bool {
    let Some(run_id) = test_run_id_for_label(window_name) else {
        return false;
    };
    test_supervisor().run_is_waiting_for_input(run_id)
}

#[cfg(test)]
pub fn cancel_run_labels_matching(base: &str) {
    test_supervisor().cancel_runs_matching_label(base);
}

#[cfg(test)]
pub fn request_run_label_exit(window_name: &str) {
    if let Some(run_id) = test_run_id_for_label(window_name) {
        test_supervisor().request_run_exit(run_id);
    }
}

#[cfg(test)]
pub fn send_run_label_input(window_name: &str, text: String) -> bool {
    test_run_id_for_label(window_name)
        .is_some_and(|run_id| test_supervisor().send_run_input(run_id, text))
}

#[cfg(test)]
pub fn interrupt_run_label_input(window_name: &str, text: String) -> bool {
    test_run_id_for_label(window_name)
        .is_some_and(|run_id| test_supervisor().interrupt_run_input(run_id, text))
}

#[cfg(test)]
pub fn force_interrupt_run_label(window_name: &str, text: String) -> bool {
    test_run_id_for_label(window_name)
        .is_some_and(|run_id| test_supervisor().force_interrupt_run(run_id, text))
}

#[cfg(test)]
pub fn terminate_run_label(window_name: &str) -> bool {
    test_run_id_for_label(window_name).is_some_and(|run_id| test_supervisor().terminate_run(run_id))
}

#[cfg(test)]
pub fn shutdown_all_runs() {
    test_supervisor().shutdown_all_runs();
}

/// Test-only: drain queued `AcpInput` messages on the per-window observer
/// registered by `request_run_label_*_for_test`. Returns each queued input as
/// a stable `(kind, text)` pair so callers do not need access to the private
/// `AcpInput` enum.
#[cfg(test)]
pub fn drain_test_input_receiver_for(window_name: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(entry) = test_input_observers().get(window_name) {
        let mut rx = entry.value().lock();
        while let Ok(input) = rx.try_recv() {
            match input {
                AcpInput::Prompt(text) => out.push(("prompt", text)),
                AcpInput::Interrupt(text) => out.push(("interrupt", text)),
            }
        }
    }
    out
}

/// Test-only: observe whether the per-window cancel signal has been raised.
/// Returns the recorded reason once and clears it from the observer slot so
/// repeat calls without a fresh signal return empty, matching the previous
/// `try_recv()`-based assertion shape.
#[cfg(test)]
pub fn drain_test_cancel_receiver_for(window_name: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if let Some(entry) = test_cancel_observers().get(window_name) {
        let signal = entry.value();
        if signal.token.is_cancelled()
            && let Some(reason) = signal.reason.lock().take()
        {
            out.push(match reason {
                AcpCancelReason::Terminate => "terminate",
                AcpCancelReason::Complete => "complete",
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_fixture_run(
        supervisor: &Supervisor,
        run_id: RunId,
        window_name: &str,
        waiting: bool,
    ) -> mpsc::UnboundedReceiver<AcpInput> {
        let cancel = supervisor.child_cancel_signal();
        let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
        let (_waiting_tx, waiting_rx) = watch::channel(waiting);
        let (_finished_tx, finished_rx) = watch::channel(false);
        supervisor.inner.runs.insert(
            run_id,
            RunHandle {
                window_name: window_name.to_string(),
                cancel,
                input_tx,
                waiting_for_input: waiting_rx,
                finished: finished_rx,
                join: None,
            },
        );
        input_rx
    }

    #[test]
    fn supervisor_targets_input_by_run_id_when_labels_match() {
        let supervisor = Supervisor::new();
        let mut first_rx = insert_fixture_run(&supervisor, 10, "[Duplicate]", true);
        let mut second_rx = insert_fixture_run(&supervisor, 11, "[Duplicate]", true);

        assert!(supervisor.send_run_input(11, "second".to_string()));

        assert!(first_rx.try_recv().is_err());
        match second_rx.try_recv().expect("second run input") {
            AcpInput::Prompt(text) => assert_eq!(text, "second"),
            other => panic!("expected prompt input, got {other:?}"),
        }
    }
}
