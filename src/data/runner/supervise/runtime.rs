use super::launch::write_launch_cause;
use super::{CancelSignal, ManagedAcpOutcome};
use crate::acp::{
    AcpConnector, AcpRuntimeEvent, AcpSession, SubprocessConnector, translate_update,
};
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
use crate::runner::exit::{
    enforce_readonly_workspace_policy, git_rev_parse_head, git_status_porcelain,
    validate_toml_artifacts, write_finish_stamp_for_outcome,
};
use crate::runner::transport::{
    AcpCancelReason, AcpClock, AcpDiagnostics, AcpInput, AcpTextStream, ManagedAcpLaunch,
    RealAcpClock, RealAcpDiagnostics, append_acp_text_trace, find_launch_run_id,
};
use crate::state::MessageKind;
use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{collections::VecDeque, fs};
use tokio::sync::{mpsc, watch};

/// Cancel-ack watchdog phase constants.
const CANCEL_ACK_RESEND_SECS: u64 = 60;
const CANCEL_ACK_TERMINATE_SECS: u64 = 60;

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
    let session = connector
        .connect(&launch.resolved)
        .map_err(|err| anyhow!("{err}"))?;
    drive_acp_session(session, launch, cancel, input_rx, waiting_for_input)
}
/// Drive a connected ACP session through its prompt-turn lifecycle. Production
/// wrapper that passes [`RealAcpClock`] and [`RealAcpDiagnostics`].
fn drive_acp_session(
    session: Box<dyn AcpSession>,
    launch: &ManagedAcpLaunch,
    cancel: &CancelSignal,
    input_rx: &mut mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: &watch::Sender<bool>,
) -> Result<ManagedAcpOutcome> {
    drive_acp_session_with_clock(
        session,
        launch,
        cancel,
        input_rx,
        waiting_for_input,
        &RealAcpClock,
        &RealAcpDiagnostics,
    )
}

/// Clock-injectable core loop. Production passes [`RealAcpClock`] and
/// [`RealAcpDiagnostics`]; tests pass a [`FakeAcpClock`] whose perceived
/// wall-clock can be advanced deterministically across the 60s/120s
/// cancel-ack thresholds without sleeping real time, plus a recording
/// [`FakeAcpDiagnostics`] so they can assert the runtime persisted the
/// expected `SummaryWarn` messages and emitted the durable JSONL trace
/// records the operator relies on for headless postmortems.
pub(super) fn drive_acp_session_with_clock<C: AcpClock, D: AcpDiagnostics>(
    mut session: Box<dyn AcpSession>,
    launch: &ManagedAcpLaunch,
    cancel: &CancelSignal,
    input_rx: &mut mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: &watch::Sender<bool>,
    clock: &C,
    diagnostics: &D,
) -> Result<ManagedAcpOutcome> {
    let mut agent_text = AcpTextStream::new();
    let mut thought_text = AcpTextStream::new();
    let mut pending_input = VecDeque::new();
    let mut waiting_for_interactive_prompt = false;
    let mut interrupting_turn = false;
    let mut watchdog = CancelAckWatchdog::new();
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
                        // A second :interrupt while a cancel is already pending
                        // appends to pending_input but does NOT re-arm the
                        // watchdog timers.
                        watchdog.arm(clock.now());
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
            watchdog.clear();
        }
        let event = session
            .try_next_update()
            .map_err(|err| anyhow!("{err}"))?
            .and_then(|update| translate_update(update, launch.resolved.interactive));
        // If the underlying transport (e.g., the ACP child process) has gone
        // away without emitting a terminal event, synthesize a
        // PromptTurnFailed so the loop can route through the existing
        // failure paths instead of hanging on `Ok(None)` polls forever.
        let event = match event {
            Some(e) => Some(e),
            None => session
                .dead_reason()
                .map_err(|err| anyhow!("{err}"))?
                .map(|message| AcpRuntimeEvent::PromptTurnFailed { message }),
        };
        match event {
            Some(AcpRuntimeEvent::PromptTurnFinished) => {
                thought_text.finish_turn(launch, MessageKind::AgentThought);
                agent_text.finish_turn(launch, MessageKind::AgentText);
                watchdog.clear();
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
                    clock.park();
                    watchdog.tick(&mut session, launch, cancel, clock, diagnostics)?;
                    continue;
                }
                // Non-interactive resubmit: when interrupting_turn is set
                // and pending text is queued, submit that text as the next
                // turn instead of closing with exit_code 0. This fixes the
                // race where the vendor's turn finishes between the
                // interrupt being queued and cancel propagation completing.
                if interrupting_turn && let Some(text) = pending_input.pop_front() {
                    // `queued` reports how many interrupt texts remain queued
                    // *after* the resubmitted text has been popped; 0 means
                    // this resubmit fully drained the interrupt buffer.
                    let queued = pending_input.len();
                    let _ = waiting_for_input.send_replace(false);
                    session
                        .submit_prompt(&text)
                        .map_err(|err| anyhow!("{err}"))?;
                    interrupting_turn = false;
                    diagnostics.record_event(
                        launch,
                        serde_json::json!({
                            "type": "acp_resubmit_on_finished",
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "run": find_launch_run_id(launch),
                            "window": launch.window_name,
                            "queued": queued,
                        }),
                    );
                    clock.park();
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
            Some(AcpRuntimeEvent::PromptTurnFailed { .. }) => {
                thought_text.finish_turn(launch, MessageKind::AgentThought);
                agent_text.finish_turn(launch, MessageKind::AgentText);
                watchdog.clear();
                // Watchdog warnings (and any other interrupt-with-text)
                // cancel the in-flight ACP turn and leave the resumption text
                // queued in pending_input. If we got here from such an
                // interrupt — interactive or not — resubmit the queued text
                // as the next prompt so the agent can act on the warning and
                // run to natural completion.
                if interrupting_turn && let Some(text) = pending_input.pop_front() {
                    let _ = waiting_for_input.send_replace(false);
                    session
                        .submit_prompt(&text)
                        .map_err(|err| anyhow!("{err}"))?;
                    interrupting_turn = false;
                    if launch.resolved.interactive {
                        waiting_for_interactive_prompt = false;
                    }
                    clock.park();
                    watchdog.tick(&mut session, launch, cancel, clock, diagnostics)?;
                    continue;
                }
                if launch.resolved.interactive && interrupting_turn {
                    waiting_for_interactive_prompt = true;
                    interrupting_turn = false;
                    let _ = waiting_for_input.send_replace(true);
                    clock.park();
                    watchdog.tick(&mut session, launch, cancel, clock, diagnostics)?;
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
                clock.park();
            }
            Some(AcpRuntimeEvent::ToolCallActivity { .. })
            | Some(AcpRuntimeEvent::SessionTitleUpdated { .. })
            | None => clock.park(),
        }
        watchdog.tick(&mut session, launch, cancel, clock, diagnostics)?;
    };
    Ok(outcome)
}

/// Local cancel-ack watchdog state. Tracks when the first cancel was sent
/// to the ACP session and, after a resend, when that resend went out — so
/// the loop can escalate to `AcpCancelReason::Terminate` if the vendor
/// never acknowledges the cancel.
struct CancelAckWatchdog {
    sent_at: Option<Instant>,
    resend_at: Option<Instant>,
}

impl CancelAckWatchdog {
    fn new() -> Self {
        Self {
            sent_at: None,
            resend_at: None,
        }
    }

    fn arm(&mut self, now: Instant) {
        self.sent_at = Some(now);
        self.resend_at = None;
    }

    fn clear(&mut self) {
        self.sent_at = None;
        self.resend_at = None;
    }

    /// Evaluate the watchdog. If the first 60-second window has elapsed
    /// since the initial cancel, resend `session.cancel_prompt()` once,
    /// emit a `SummaryWarn` dashboard message, log `acp_cancel_resent`,
    /// and arm a second 60-second window. If that second window also
    /// elapses without a terminal ACP event, emit a second `SummaryWarn`,
    /// log `acp_cancel_timeout_terminate`, and signal
    /// `AcpCancelReason::Terminate` so the existing exit_code 143 /
    /// vendor-failover path takes over on the next loop iteration.
    fn tick<C: AcpClock, D: AcpDiagnostics>(
        &mut self,
        session: &mut Box<dyn AcpSession>,
        launch: &ManagedAcpLaunch,
        cancel: &CancelSignal,
        clock: &C,
        diagnostics: &D,
    ) -> Result<()> {
        let Some(sent_at) = self.sent_at else {
            return Ok(());
        };
        let now = clock.now();
        let elapsed = now.duration_since(sent_at);
        if self.resend_at.is_none() && elapsed >= Duration::from_secs(CANCEL_ACK_RESEND_SECS) {
            // First 60 s expiry: resend cancel once and arm second window.
            session.cancel_prompt().map_err(|err| anyhow!("{err}"))?;
            self.resend_at = Some(now);
            let msg = "ACP cancel not acknowledged after 60s; resending cancel. If still unresponsive after another 60s the run will be terminated.";
            diagnostics.persist_warning(launch, msg);
            diagnostics.record_event(
                launch,
                serde_json::json!({
                    "type": "acp_cancel_resent",
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "run": find_launch_run_id(launch),
                    "window": launch.window_name,
                    "elapsed_ms": elapsed.as_millis(),
                }),
            );
            return Ok(());
        }
        if let Some(resend_at) = self.resend_at
            && now.duration_since(resend_at) >= Duration::from_secs(CANCEL_ACK_TERMINATE_SECS)
        {
            // Second 60 s expiry: escalate to Terminate so the existing
            // vendor-failover path takes over.
            let msg = "ACP cancel still not acknowledged after 120s; terminating run, vendor failover will follow.";
            diagnostics.persist_warning(launch, msg);
            diagnostics.record_event(
                launch,
                serde_json::json!({
                    "type": "acp_cancel_timeout_terminate",
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "run": find_launch_run_id(launch),
                    "window": launch.window_name,
                    "elapsed_ms": elapsed.as_millis(),
                }),
            );
            cancel.signal(AcpCancelReason::Terminate);
            self.clear();
        }
        Ok(())
    }
}

pub(super) async fn finalize_managed_acp_launch(
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
            let _ = write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, 1, "")
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
            let _ = write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, 1, "")
                .await;
        }
    }
}
