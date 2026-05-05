//! Supervises managed ACP runs: builds launch contexts, drives the per-run
//! polling loop, owns the global active-run registry, and exposes the public
//! control surface the orchestrator uses to start, interrupt, and tear down
//! agent runs.
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
use std::collections::VecDeque;
use std::{
    fs,
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use super::exit::{
    enforce_readonly_workspace_policy, git_rev_parse_head, git_status_porcelain,
    validate_toml_artifacts, write_finish_stamp_for_outcome,
};
use super::transport::{
    ACP_POLL_INTERVAL, AcpCancelReason, AcpInput, AcpTextStream, ManagedAcpLaunch,
    ToolCallTransition, acp_trace_path_from_cause_path, append_acp_text_trace,
};

#[derive(Debug)]
pub(in crate::data::runner) struct ManagedAcpRun {
    pub(in crate::data::runner) cancel_tx: mpsc::Sender<AcpCancelReason>,
    pub(in crate::data::runner) input_tx: mpsc::Sender<AcpInput>,
    /// Receives lifecycle transitions emitted by the runner thread. The
    /// App drains this on its main poll cadence and applies them to the
    /// per-run watchdog state. Held inside the mutex-protected
    /// `active_acp_runs()` map, so consumers must lock before draining.
    pub(in crate::data::runner) tool_call_transition_rx: mpsc::Receiver<ToolCallTransition>,
    pub(in crate::data::runner) finished: std::sync::Arc<AtomicBool>,
    pub(in crate::data::runner) waiting_for_input: std::sync::Arc<AtomicBool>,
    pub(in crate::data::runner) join: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
struct ManagedAcpOutcome {
    exit_code: i32,
    signal_received: String,
}

pub(in crate::data::runner) fn active_acp_runs()
-> &'static Mutex<std::collections::HashMap<String, ManagedAcpRun>> {
    static ACTIVE: OnceLock<Mutex<std::collections::HashMap<String, ManagedAcpRun>>> =
        OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(in crate::data::runner) fn test_input_receivers()
-> &'static Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpInput>>> {
    static RECEIVERS: OnceLock<Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpInput>>>> =
        OnceLock::new();
    RECEIVERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(in crate::data::runner) fn test_cancel_receivers()
-> &'static Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpCancelReason>>> {
    static RECEIVERS: OnceLock<
        Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpCancelReason>>>,
    > = OnceLock::new();
    RECEIVERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
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
    cancel_rx: mpsc::Receiver<AcpCancelReason>,
    input_rx: mpsc::Receiver<AcpInput>,
    transition_tx: mpsc::Sender<ToolCallTransition>,
    waiting_for_input: std::sync::Arc<AtomicBool>,
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
        if let Ok(reason) = cancel_rx.try_recv() {
            waiting_for_input.store(false, Ordering::SeqCst);
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
                        waiting_for_input.store(false, Ordering::SeqCst);
                    }
                }
            }
        }

        if waiting_for_interactive_prompt && let Some(text) = pending_input.pop_front() {
            waiting_for_input.store(false, Ordering::SeqCst);
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
                        waiting_for_input.store(false, Ordering::SeqCst);
                        session
                            .submit_prompt(&text)
                            .map_err(|err| anyhow!("{err}"))?;
                        waiting_for_interactive_prompt = false;
                        interrupting_turn = false;
                    } else {
                        waiting_for_interactive_prompt = true;
                        interrupting_turn = false;
                        waiting_for_input.store(true, Ordering::SeqCst);
                    }
                    thread::sleep(ACP_POLL_INTERVAL);
                    continue;
                }
                waiting_for_input.store(false, Ordering::SeqCst);
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
                        waiting_for_input.store(false, Ordering::SeqCst);
                        session
                            .submit_prompt(&text)
                            .map_err(|err| anyhow!("{err}"))?;
                        waiting_for_interactive_prompt = false;
                        interrupting_turn = false;
                    } else {
                        waiting_for_interactive_prompt = true;
                        interrupting_turn = false;
                        waiting_for_input.store(true, Ordering::SeqCst);
                    }
                    thread::sleep(ACP_POLL_INTERVAL);
                    continue;
                }
                waiting_for_input.store(false, Ordering::SeqCst);
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
                thread::sleep(ACP_POLL_INTERVAL)
            }
            Some(AcpRuntimeEvent::ToolCallActivity { tool_call_id, kind }) => {
                // Stamp `observed_at` at the moment the runner saw the
                // transition. The send is best-effort: if the receiver has
                // been dropped (e.g. App teardown), a missing pause/resume
                // signal is preferable to crashing the runner.
                let _ = transition_tx.send(ToolCallTransition {
                    tool_call_id,
                    kind,
                    observed_at: Instant::now(),
                });
            }
            Some(AcpRuntimeEvent::Lifecycle(_)) | None => thread::sleep(ACP_POLL_INTERVAL),
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

fn finalize_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel_rx: mpsc::Receiver<AcpCancelReason>,
    input_rx: mpsc::Receiver<AcpInput>,
    transition_tx: mpsc::Sender<ToolCallTransition>,
    waiting_for_input: std::sync::Arc<AtomicBool>,
) {
    match run_managed_acp_launch(
        launch.clone(),
        cancel_rx,
        input_rx,
        transition_tx,
        std::sync::Arc::clone(&waiting_for_input),
    ) {
        Ok(_) => {
            waiting_for_input.store(false, Ordering::SeqCst);
            let _ = fs::remove_file(&launch.cause_path);
            return;
        }
        Err(err) => {
            waiting_for_input.store(false, Ordering::SeqCst);
            let _ = write_launch_cause(&launch.cause_path, &format!("{err:#}"));
        }
    }

    let fallback_head_before = git_rev_parse_head().unwrap_or_default();
    let _ = write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, 1, "");
}

fn cleanup_finished_acp_runs() {
    let mut finished = Vec::new();
    {
        let guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (window_name, run) in guard.iter() {
            if run.finished.load(Ordering::SeqCst) {
                finished.push(window_name.clone());
            }
        }
    }
    for window_name in finished {
        if let Some(mut run) = take_managed_acp_run(&window_name)
            && let Some(handle) = run.join.take()
        {
            let _ = handle.join();
        }
    }
}

fn take_managed_acp_run(window_name: &str) -> Option<ManagedAcpRun> {
    #[cfg(test)]
    {
        test_input_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(window_name);
    }
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(window_name)
}

fn launch_managed_acp_window(window_name: &str, launch: ManagedAcpLaunch) -> Result<()> {
    cleanup_finished_acp_runs();

    let mut guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .values()
        .any(|run| !run.finished.load(Ordering::SeqCst))
    {
        bail!("codexize only supports one active ACP run at a time");
    }

    let (cancel_tx, cancel_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel();
    let (transition_tx, transition_rx) = mpsc::channel();
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let waiting_for_input = std::sync::Arc::new(AtomicBool::new(false));
    let finished_flag = std::sync::Arc::clone(&finished);
    let waiting_for_input_flag = std::sync::Arc::clone(&waiting_for_input);
    let launch_window = window_name.to_string();
    let handle = thread::spawn(move || {
        finalize_managed_acp_launch(
            launch,
            cancel_rx,
            input_rx,
            transition_tx,
            waiting_for_input_flag,
        );
        finished_flag.store(true, Ordering::SeqCst);
    });
    guard.insert(
        launch_window,
        ManagedAcpRun {
            cancel_tx,
            input_tx,
            tool_call_transition_rx: transition_rx,
            finished,
            waiting_for_input,
            join: Some(handle),
        },
    );
    Ok(())
}

pub fn run_label_is_active(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(window_name)
        .is_some_and(|run| !run.finished.load(Ordering::SeqCst))
}

pub fn run_label_is_waiting_for_input(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(window_name)
        .is_some_and(|run| {
            !run.finished.load(Ordering::SeqCst) && run.waiting_for_input.load(Ordering::SeqCst)
        })
}

#[cfg(test)]
pub fn request_run_label_interactive_input_for_test(window_name: &str) {
    request_run_label_for_test(window_name, true);
}

#[cfg(test)]
pub fn request_run_label_active_for_test(window_name: &str) {
    request_run_label_for_test(window_name, false);
}

/// Test-only: drain queued `AcpInput` messages on the per-window test
/// receiver registered by `request_run_label_*_for_test`. Returns each
/// queued input as a stable `(kind, text)` pair so callers do not need
/// access to the private `AcpInput` enum.
#[cfg(test)]
pub fn drain_test_input_receiver_for(window_name: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let map = test_input_receivers();
    let guard = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(rx) = guard.get(window_name) {
        while let Ok(input) = rx.try_recv() {
            match input {
                AcpInput::Prompt(text) => out.push(("prompt", text)),
                AcpInput::Interrupt(text) => out.push(("interrupt", text)),
            }
        }
    }
    out
}

/// Test-only: drain queued `AcpCancelReason` messages on the per-window
/// test receiver. Returns each reason as a stable string so callers do
/// not need access to the private `AcpCancelReason` enum.
#[cfg(test)]
pub fn drain_test_cancel_receiver_for(window_name: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let map = test_cancel_receivers();
    let guard = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(rx) = guard.get(window_name) {
        while let Ok(reason) = rx.try_recv() {
            out.push(match reason {
                AcpCancelReason::Terminate => "terminate",
                AcpCancelReason::Complete => "complete",
            });
        }
    }
    out
}

#[cfg(test)]
fn request_run_label_for_test(window_name: &str, waiting: bool) {
    let mut guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel();
    let (_transition_tx, transition_rx) = mpsc::channel();
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let waiting_for_input = std::sync::Arc::new(AtomicBool::new(waiting));
    test_input_receivers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(window_name.to_string(), input_rx);
    test_cancel_receivers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(window_name.to_string(), cancel_rx);
    guard.insert(
        window_name.to_string(),
        ManagedAcpRun {
            cancel_tx,
            input_tx,
            tool_call_transition_rx: transition_rx,
            finished,
            waiting_for_input,
            join: None,
        },
    );
}

pub fn cancel_run_labels_matching(base: &str) {
    let prefix = format!("{base} ");
    let matching = {
        let guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .keys()
            .filter(|name| *name == base || name.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>()
    };

    for window_name in matching {
        if let Some(mut run) = take_managed_acp_run(&window_name) {
            let _ = run.cancel_tx.send(AcpCancelReason::Terminate);
            if let Some(handle) = run.join.take() {
                let _ = handle.join();
            }
        }
    }
}

pub fn request_run_label_exit(window_name: &str) {
    if let Some(mut run) = take_managed_acp_run(window_name) {
        let _ = run.cancel_tx.send(AcpCancelReason::Complete);
        if let Some(handle) = run.join.take() {
            let _ = handle.join();
        }
    }
}

pub fn send_run_label_input(window_name: &str, text: String) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(window_name)
        .filter(|run| {
            !run.finished.load(Ordering::SeqCst) && run.waiting_for_input.load(Ordering::SeqCst)
        })
        .is_some_and(|run| run.input_tx.send(AcpInput::Prompt(text)).is_ok())
}

pub fn interrupt_run_label_input(window_name: &str, text: String) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(window_name).is_some_and(|run| {
        if run.finished.load(Ordering::SeqCst) {
            return false;
        }
        let input = if run.waiting_for_input.load(Ordering::SeqCst) {
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
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(window_name).is_some_and(|run| {
        if run.finished.load(Ordering::SeqCst) {
            return false;
        }
        run.input_tx.send(AcpInput::Interrupt(text)).is_ok()
    })
}

/// Best-effort `AcpCancelReason::Terminate` for the named run (spec §3.5).
/// Unlike `cancel_run_labels_matching`, this does not remove the run from
/// the active map or join the runner thread — the existing
/// `poll_agent_run` finalize path observes `!active_run_exists` once the
/// runner thread exits and routes the non-zero exit through the standard
/// failed-run vendor failover. Returns `false` if no such run is active.
pub fn terminate_run_label(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(window_name).is_some_and(|run| {
        if run.finished.load(Ordering::SeqCst) {
            return false;
        }
        run.cancel_tx.send(AcpCancelReason::Terminate).is_ok()
    })
}

/// Drain all queued tool-call lifecycle transitions across every managed
/// ACP run currently active. Returned in `(window_name, transition)` pairs
/// in arrival order per run; cross-run interleaving is preserved by the
/// timestamp on each transition. Callers should apply transitions in
/// `observed_at` order.
fn drain_tool_call_transitions() -> Vec<(String, ToolCallTransition)> {
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut out = Vec::new();
    for (window_name, run) in guard.iter() {
        while let Ok(transition) = run.tool_call_transition_rx.try_recv() {
            out.push((window_name.clone(), transition));
        }
    }
    out
}

/// Drain queued tool-call lifecycle transitions as typed [`DataEvent`]s. The
/// runtime uses this as its only entry point so the registry's per-run
/// channels stay an internal detail of `data/runner`.
pub fn drain_tool_call_events() -> Vec<crate::data::events::DataEvent> {
    drain_tool_call_transitions()
        .into_iter()
        .map(
            |(window_name, transition)| crate::data::events::DataEvent::ToolCallTransition {
                window_name,
                transition,
            },
        )
        .collect()
}

pub fn shutdown_all_runs() {
    let runs = {
        let mut guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
            .into_values()
            .collect::<Vec<_>>()
    };
    #[cfg(test)]
    {
        test_input_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        test_cancel_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    for mut run in runs {
        let _ = run.cancel_tx.send(AcpCancelReason::Terminate);
        if let Some(handle) = run.join.take() {
            let _ = handle.join();
        }
    }
}

/// Build a managed launch context and register it on the active-run map.
/// The runner-root module exposes the public `launch_interactive` /
/// `launch_noninteractive` entrypoints that wrap this; callers outside the
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
