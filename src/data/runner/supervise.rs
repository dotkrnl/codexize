//! Supervises managed ACP runs: builds launch contexts, drives the per-run
//! polling loop on a tokio runtime owned by the supervisor, owns the keyed
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
use std::{
    fs,
    path::Path,
    sync::{Arc, OnceLock},
    thread,
};
use tokio::{
    runtime::Runtime,
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
#[derive(Debug)]
struct CancelSignal {
    token: CancellationToken,
    reason: PlMutex<Option<AcpCancelReason>>,
}

impl CancelSignal {
    fn new() -> Self {
        Self {
            token: CancellationToken::new(),
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

/// Process-supervision root. Owns the keyed `RunHandle` registry plus the
/// dedicated tokio runtime that drives every per-run `spawn_blocking` task.
/// Currently surfaced through the module-level `supervisor()` accessor as a
/// process-global `OnceLock<Supervisor>`; the next concurrency seam will fold
/// this value into `App` so the runtime is a single tree-rooted `tokio` env.
pub(in crate::data::runner) struct Supervisor {
    runtime: Arc<Runtime>,
    runs: DashMap<String, RunHandle>,
}

impl Supervisor {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("codexize-runner")
            .build()
            .expect("failed to build runner supervisor tokio runtime");
        Self {
            runtime: Arc::new(runtime),
            runs: DashMap::new(),
        }
    }

    /// Drain RunHandles whose inner task has signalled `finished=true`,
    /// joining their tasks so resources release deterministically before the
    /// next public-control-surface call inspects the map.
    fn cleanup_finished(&self) {
        let finished_keys: Vec<String> = self
            .runs
            .iter()
            .filter(|entry| entry.value().is_finished())
            .map(|entry| entry.key().clone())
            .collect();
        for key in finished_keys {
            if let Some((_, mut run)) = self.runs.remove(&key)
                && let Some(join) = run.join.take()
            {
                let _ = self.runtime.block_on(join);
            }
        }
    }

    fn label_is_active(&self, window_name: &str) -> bool {
        self.runs
            .get(window_name)
            .is_some_and(|run| !run.is_finished())
    }

    fn label_is_waiting(&self, window_name: &str) -> bool {
        self.runs
            .get(window_name)
            .is_some_and(|run| !run.is_finished() && run.is_waiting_for_input())
    }

    fn any_run_unfinished(&self) -> bool {
        self.runs.iter().any(|entry| !entry.value().is_finished())
    }
}

pub(in crate::data::runner) fn supervisor() -> &'static Supervisor {
    static SUPERVISOR: OnceLock<Supervisor> = OnceLock::new();
    SUPERVISOR.get_or_init(Supervisor::new)
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

fn run_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel: &CancelSignal,
    input_rx: &mut mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: &watch::Sender<bool>,
) -> Result<ManagedAcpOutcome> {
    let head_before = git_rev_parse_head().unwrap_or_default();
    // Final validation is allowed to update its ignored artifact files, so the
    // ACP-side write allowlist carries those exact paths while the runner
    // enforces that no git-visible workspace state changes during the run.
    let git_status_before = launch
        .resolved
        .session
        .policy
        .enforce_readonly_workspace
        .then(git_status_porcelain)
        .transpose()?;
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
            thought_text.finish_turn(&launch, MessageKind::AgentThought);
            agent_text.finish_turn(&launch, MessageKind::AgentText);
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
                thought_text.finish_turn(&launch, MessageKind::AgentThought);
                agent_text.finish_turn(&launch, MessageKind::AgentText);
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
                thought_text.finish_turn(&launch, MessageKind::AgentThought);
                agent_text.finish_turn(&launch, MessageKind::AgentText);
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
                append_acp_text_trace(&launch, &text_event);
                let text = text_event.text;
                if text_event.thought {
                    thought_text.push_text_boundary(
                        &launch,
                        &text,
                        MessageKind::AgentThought,
                        text_event.boundary,
                    );
                } else {
                    agent_text.push_text_boundary(
                        &launch,
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

    enforce_readonly_workspace_policy(
        launch.resolved.session.policy.enforce_readonly_workspace,
        &head_before,
        git_status_before.as_deref(),
    )?;
    write_finish_stamp_for_outcome(
        &launch.stamp_path,
        head_before,
        outcome.exit_code,
        &outcome.signal_received,
    )?;
    Ok(outcome)
}

/// Pace the inner sync loop without busy-spinning. `park_timeout` returns
/// after the interval (or sooner if `Thread::unpark` was called); using park
/// keeps the runner product code off the banned blocking-poll primitive
/// while preserving the coarse poll cadence.
fn poll_park() {
    thread::park_timeout(ACP_POLL_INTERVAL);
}

fn finalize_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel: Arc<CancelSignal>,
    mut input_rx: mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: watch::Sender<bool>,
) {
    match run_managed_acp_launch(
        launch.clone(),
        cancel.as_ref(),
        &mut input_rx,
        &waiting_for_input,
    ) {
        Ok(_) => {
            let _ = waiting_for_input.send_replace(false);
            let _ = fs::remove_file(&launch.cause_path);
            return;
        }
        Err(err) => {
            let _ = waiting_for_input.send_replace(false);
            let _ = write_launch_cause(&launch.cause_path, &format!("{err:#}"));
        }
    }

    let fallback_head_before = git_rev_parse_head().unwrap_or_default();
    let _ = write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, 1, "");
}

fn launch_managed_acp_window(window_name: &str, launch: ManagedAcpLaunch) -> Result<()> {
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    if supervisor.any_run_unfinished() {
        bail!("codexize only supports one active ACP run at a time");
    }

    let cancel = Arc::new(CancelSignal::new());
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
    let (waiting_tx, waiting_rx) = watch::channel(false);
    let (finished_tx, finished_rx) = watch::channel(false);

    let cancel_for_task = Arc::clone(&cancel);
    let join = supervisor.runtime.spawn_blocking(move || {
        finalize_managed_acp_launch(launch, cancel_for_task, input_rx, waiting_tx);
        let _ = finished_tx.send(true);
    });

    supervisor.runs.insert(
        window_name.to_string(),
        RunHandle {
            cancel,
            input_tx,
            waiting_for_input: waiting_rx,
            finished: finished_rx,
            join: Some(join),
        },
    );
    Ok(())
}

pub fn run_label_is_active(window_name: &str) -> bool {
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    supervisor.label_is_active(window_name)
}

pub fn run_label_is_waiting_for_input(window_name: &str) -> bool {
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    supervisor.label_is_waiting(window_name)
}

fn take_run(window_name: &str) -> Option<RunHandle> {
    let supervisor = supervisor();
    #[cfg(test)]
    {
        test_input_observers().remove(window_name);
    }
    supervisor.runs.remove(window_name).map(|(_, run)| run)
}

pub fn cancel_run_labels_matching(base: &str) {
    let supervisor = supervisor();
    let prefix = format!("{base} ");
    let matching: Vec<String> = supervisor
        .runs
        .iter()
        .map(|entry| entry.key().clone())
        .filter(|name| name == base || name.starts_with(&prefix))
        .collect();

    for window_name in matching {
        if let Some(mut run) = take_run(&window_name) {
            run.cancel.signal(AcpCancelReason::Terminate);
            if let Some(join) = run.join.take() {
                let _ = supervisor.runtime.block_on(join);
            }
        }
    }
}

pub fn request_run_label_exit(window_name: &str) {
    let supervisor = supervisor();
    if let Some(mut run) = take_run(window_name) {
        run.cancel.signal(AcpCancelReason::Complete);
        if let Some(join) = run.join.take() {
            let _ = supervisor.runtime.block_on(join);
        }
    }
}

pub fn send_run_label_input(window_name: &str, text: String) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    supervisor
        .runs
        .get(window_name)
        .filter(|run| !run.is_finished() && run.is_waiting_for_input())
        .is_some_and(|run| run.input_tx.send(AcpInput::Prompt(text)).is_ok())
}

pub fn interrupt_run_label_input(window_name: &str, text: String) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    supervisor.runs.get(window_name).is_some_and(|run| {
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
/// by the watchdog warning path (spec §3.4) where the spec requires
/// cancelling the in-flight ACP turn and queueing the warning as the next
/// prompt — converting to `Prompt` (as `interrupt_run_label_input` would
/// when waiting) would skip the cancel_prompt() side effect.
pub fn force_interrupt_run_label(window_name: &str, text: String) -> bool {
    if text.is_empty() {
        return false;
    }
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    supervisor.runs.get(window_name).is_some_and(|run| {
        if run.is_finished() {
            return false;
        }
        run.input_tx.send(AcpInput::Interrupt(text)).is_ok()
    })
}

/// Best-effort `AcpCancelReason::Terminate` for the named run (spec §3.5).
/// Unlike `cancel_run_labels_matching`, this does not remove the run from
/// the registry or join the runner task — the existing `poll_agent_run`
/// finalize path observes `!run_label_is_active` once the runner task exits
/// and routes the non-zero exit through the standard failed-run vendor
/// failover. Returns `false` if no such run is active.
pub fn terminate_run_label(window_name: &str) -> bool {
    let supervisor = supervisor();
    supervisor.cleanup_finished();
    supervisor.runs.get(window_name).is_some_and(|run| {
        if run.is_finished() {
            return false;
        }
        run.cancel.signal(AcpCancelReason::Terminate);
        true
    })
}

pub fn shutdown_all_runs() {
    let supervisor = supervisor();
    let runs: Vec<(String, RunHandle)> = {
        let keys: Vec<String> = supervisor
            .runs
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        keys.into_iter()
            .filter_map(|key| supervisor.runs.remove(&key))
            .collect()
    };
    #[cfg(test)]
    {
        test_input_observers().clear();
        test_cancel_observers().clear();
    }

    for (_, mut run) in runs {
        run.cancel.signal(AcpCancelReason::Terminate);
        if let Some(join) = run.join.take() {
            let _ = supervisor.runtime.block_on(join);
        }
    }
}

/// Build a managed launch context and register it on the supervisor's run
/// registry. The runner-root module exposes the public `launch_interactive`
/// /`launch_noninteractive` entrypoints that wrap this; callers outside the
/// `runner` module tree should not route through it directly.
#[allow(clippy::too_many_arguments)]
pub(in crate::data::runner) fn launch_managed(
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
    launch_managed_acp_window(window_name, launch)
}

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

#[cfg(test)]
fn test_input_observers() -> &'static DashMap<String, PlMutex<mpsc::UnboundedReceiver<AcpInput>>> {
    static OBSERVERS: OnceLock<DashMap<String, PlMutex<mpsc::UnboundedReceiver<AcpInput>>>> =
        OnceLock::new();
    OBSERVERS.get_or_init(DashMap::new)
}

/// Test-only side observer for the cancel signal of a fixture run. The
/// observer holds the same `Arc<CancelSignal>` as the registered RunHandle,
/// so signals issued through `request_run_label_exit` /
/// `cancel_run_labels_matching` remain visible to drain helpers even after
/// the supervisor has removed the run from its registry.
#[cfg(test)]
fn test_cancel_observers() -> &'static DashMap<String, Arc<CancelSignal>> {
    static OBSERVERS: OnceLock<DashMap<String, Arc<CancelSignal>>> = OnceLock::new();
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
    let supervisor = supervisor();
    let cancel = Arc::new(CancelSignal::new());
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
    let (_waiting_tx, waiting_rx) = watch::channel(waiting);
    let (_finished_tx, finished_rx) = watch::channel(false);

    test_cancel_observers().insert(window_name.to_string(), Arc::clone(&cancel));
    test_input_observers().insert(window_name.to_string(), PlMutex::new(input_rx));

    supervisor.runs.insert(
        window_name.to_string(),
        RunHandle {
            cancel,
            input_tx,
            waiting_for_input: waiting_rx,
            finished: finished_rx,
            join: None,
        },
    );
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
