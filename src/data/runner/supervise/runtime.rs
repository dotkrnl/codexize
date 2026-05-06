use super::launch::write_launch_cause;
use super::{CancelSignal, ManagedAcpOutcome};
use crate::acp::{AcpConnector, AcpRuntimeEvent, AcpSession, SubprocessConnector, translate_update};
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
use crate::runner::exit::{
    enforce_readonly_workspace_policy, git_rev_parse_head, git_status_porcelain,
    validate_toml_artifacts, write_finish_stamp_for_outcome,
};
use crate::runner::transport::{
    ACP_POLL_INTERVAL, AcpCancelReason, AcpInput, AcpTextStream, ManagedAcpLaunch,
    append_acp_text_trace,
};
use crate::state::MessageKind;
use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::{collections::VecDeque, fs, thread};
use tokio::sync::{mpsc, watch};
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
/// Drive a connected ACP session through its prompt-turn lifecycle. Extracted
/// so tests can inject a fake `AcpSession` and exercise the cancel/interrupt
/// paths without spinning up a real subprocess connector.
fn drive_acp_session(
    mut session: Box<dyn AcpSession>,
    launch: &ManagedAcpLaunch,
    cancel: &CancelSignal,
    input_rx: &mut mpsc::UnboundedReceiver<AcpInput>,
    waiting_for_input: &watch::Sender<bool>,
) -> Result<ManagedAcpOutcome> {
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
        // If the underlying transport (e.g., the ACP child process) has gone
        // away without emitting a terminal event, synthesize a
        // PromptTurnFailed so the loop can route through the existing
        // failure paths instead of hanging on `Ok(None)` polls forever.
        let event = match event {
            Some(e) => Some(e),
            None => match session.dead_reason().map_err(|err| anyhow!("{err}"))? {
                Some(message) => Some(AcpRuntimeEvent::PromptTurnFailed { message }),
                None => None,
            },
        };
        match event {
            Some(AcpRuntimeEvent::PromptTurnFinished) => {
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
            Some(AcpRuntimeEvent::PromptTurnFailed { .. }) => {
                thought_text.finish_turn(launch, MessageKind::AgentThought);
                agent_text.finish_turn(launch, MessageKind::AgentText);
                // Watchdog warnings (and any other interrupt-with-text)
                // cancel the in-flight ACP turn and leave the resumption text
                // queued in pending_input. If we got here from such an
                // interrupt — interactive or not — resubmit the queued text
                // as the next prompt so the agent can act on the warning and
                // run to natural completion. Without this, non-interactive
                // runs just exit(1) and the warning text is silently lost.
                if interrupting_turn && let Some(text) = pending_input.pop_front() {
                    let _ = waiting_for_input.send_replace(false);
                    session
                        .submit_prompt(&text)
                        .map_err(|err| anyhow!("{err}"))?;
                    interrupting_turn = false;
                    if launch.resolved.interactive {
                        waiting_for_interactive_prompt = false;
                    }
                    poll_park();
                    continue;
                }
                if launch.resolved.interactive && interrupting_turn {
                    waiting_for_interactive_prompt = true;
                    interrupting_turn = false;
                    let _ = waiting_for_input.send_replace(true);
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
            | Some(AcpRuntimeEvent::SessionTitleUpdated { .. })
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
