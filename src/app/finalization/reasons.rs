use anyhow::Result;

use super::OperatorTerminationMarker;
use crate::app::{App, guard};
use crate::app::prompts::{read_review_scope_base_sha, validate_stage_toml_writes};
use crate::artifacts::{RecoveryArtifact, ReviewStatus as RecoveryStatus};
use crate::state::{
    self as session_state, MessageKind, PendingGuardDecision, PipelineItemStatus,
};
use crate::{coder_summary, final_validation, review, tasks};

impl App {
    pub(in crate::app) fn failed_unverified_reason(
        stamp_path: &std::path::Path,
        detail: impl AsRef<str>,
    ) -> String {
        format!(
            "failed_unverified: {} at {}",
            detail.as_ref(),
            stamp_path.display()
        )
    }

    pub(in crate::app) fn coder_gate_reason(
        &self,
        run: &crate::state::RunRecord,
        round_dir: &std::path::Path,
    ) -> Option<String> {
        let scope_file = round_dir.join("review_scope.toml");
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return (!Self::artifact_present(&scope_file)).then(|| "base_missing".to_string());
        }
        if !Self::artifact_present(&scope_file) {
            return Some("base_missing".to_string());
        }
        let base = match read_review_scope_base_sha(&scope_file) {
            Ok(s) => s,
            Err(_) => return Some("base_missing".to_string()),
        };
        if base.is_empty() {
            return Some("base_missing".to_string());
        }
        let stamp_path = self.finish_stamp_path_for(run);
        if !Self::artifact_present(&stamp_path) {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "missing finish stamp",
            ));
        }
        let stamp = match crate::runner::read_finish_stamp(&stamp_path) {
            Ok(stamp) => stamp,
            Err(_) => {
                return Some(Self::failed_unverified_reason(
                    &stamp_path,
                    "malformed finish stamp",
                ));
            }
        };
        if stamp.head_state != "stable" {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                format!("head_state={}", stamp.head_state),
            ));
        }
        if !stamp.working_tree_clean {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "working tree not clean on exit",
            ));
        }
        if stamp.exit_code == 0 && stamp.head_after.trim().is_empty() {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "empty stable head_after",
            ));
        }
        let summary_path = round_dir.join("coder_summary.toml");
        if summary_path.exists() {
            let summary = match coder_summary::validate(&summary_path) {
                Ok(summary) => summary,
                Err(_) => return Some("invalid_coder_summary".to_string()),
            };
            return match summary.status {
                coder_summary::CoderStatus::Done => None,
                coder_summary::CoderStatus::Partial => Some("coder_partial".to_string()),
            };
        }
        if stamp.head_after == base {
            // No commit and no coder_summary.toml. The no-commit alone is
            // legitimate when the coder declares it (status = "done", with
            // an explanation); the contract violation here is the missing
            // summary that would have explained why nothing was committed.
            return Some("missing_coder_summary".to_string());
        }
        None
    }

    /// Detect a simplifier exit that left uncommitted repository edits.
    /// The simplifier's mandatory writes (`simplification.toml` and the
    /// live-summary file) live under the gitignored session directory and
    /// therefore never appear in `git status`, so a dirty tree at exit is
    /// the sign of source edits the simplifier should have committed or
    /// reverted. No-op under the test harness.
    pub(in crate::app) fn simplifier_dirty_tree_reason(
        &self,
        run: &crate::state::RunRecord,
    ) -> Option<String> {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return None;
        }
        let stamp_path = self.finish_stamp_path_for(run);
        let Ok(stamp) = crate::runner::read_finish_stamp(&stamp_path) else {
            return None;
        };
        if stamp.working_tree_clean {
            return None;
        }
        Some(Self::failed_unverified_reason(
            &stamp_path,
            "working tree not clean on exit",
        ))
    }

    pub(in crate::app) fn normalized_failure_reason(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<Option<String>> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let (has_artifact_check, artifact_reason) = match run.stage.as_str() {
            "brainstorm" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                (
                    true,
                    (!Self::artifact_present(&spec_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "spec-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round));
                (
                    true,
                    (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "planning" => {
                let plan_path = session_dir.join("artifacts").join("plan.md");
                (
                    true,
                    (!Self::artifact_present(&plan_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "plan-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("plan-review-{}.md", run.round));
                (
                    true,
                    (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "sharding" => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let reason = if !Self::artifact_present(&tasks_path) {
                    Some("artifact_missing".to_string())
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "recovery" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                let plan_path = session_dir.join("artifacts").join("plan.md");
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let recovery_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("recovery.toml");
                let reason = if !Self::artifact_present(&spec_path)
                    || !Self::artifact_present(&plan_path)
                    || !Self::artifact_present(&tasks_path)
                    || !Self::artifact_present(&recovery_path)
                {
                    Some("artifact_missing".to_string())
                } else if let Err(err) =
                    validate_stage_toml_writes(&session_dir, "recovery", run.round)
                {
                    Some(format!("artifact_invalid: {err}"))
                } else {
                    match tasks::validate(&tasks_path) {
                        Ok(_) => self.recovery_artifact_failure_reason(&recovery_path),
                        Err(err) => Some(format!("artifact_invalid: {err}")),
                    }
                };
                (true, reason)
            }
            "coder" => {
                // Coder's real deliverable is a git commit, not a file. We
                let round_dir = session_dir.join("rounds").join(format!("{:03}", run.round));
                (false, self.coder_gate_reason(run, &round_dir))
            }
            "reviewer" => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("review.toml");
                let reason = if !Self::artifact_present(&review_path) {
                    Some("artifact_missing".to_string())
                } else {
                    review::validate(&review_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "final-validation" => {
                let verdict_path = session_dir
                    .join("artifacts")
                    .join(format!("final_validation_{}.toml", run.round));
                let reason = if !Self::artifact_present(&verdict_path) {
                    Some("artifact_missing".to_string())
                } else {
                    final_validation::validate(&verdict_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "simplifier" => {
                let simplification_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("simplification.toml");
                let reason = if !Self::artifact_present(&simplification_path) {
                    Some("artifact_missing".to_string())
                } else if let Err(err) = crate::simplification::validate(&simplification_path) {
                    Some(format!("artifact_invalid: {err}"))
                } else {
                    self.simplifier_dirty_tree_reason(run)
                };
                (true, reason)
            }
            _ => (false, None),
        };

        // If the stage produced a valid artifact, treat the run as successful
        // regardless of the wrapped pipeline's exit code. Agent commands like
        // `codex exec --json | jq ...` can return non-zero (e.g., a stray
        // non-JSON line from the agent makes jq exit 4/5) even after the
        // agent has already written a well-formed artifact. Warnings are
        // emitted for dirty-tree changes; a hard guard error (HEAD advance)
        // still fails the run.
        if has_artifact_check && artifact_reason.is_none() {
            if let Some(code) = self.read_exit_status_code(run)
                && code != 0
            {
                let _ = self.state.log_event(format!(
                    "run {} ({}) exited {code} but produced a valid artifact; treating as success",
                    run.id, run.stage
                ));
            }
            match self.enforce_run_guard(run) {
                guard::VerifyResult::Ok { warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Ok(None);
                }
                guard::VerifyResult::HardError { reason, warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Ok(Some(reason));
                }
                guard::VerifyResult::PendingDecision {
                    captured_head,
                    current_head,
                    warnings,
                } => {
                    // Park the run: populate pending decision and return Ok(None).
                    // Warnings are NOT appended yet — they replay at resolution time.
                    // The finalization caller detects the populated field and
                    // transitions to GitGuardPending instead of completing normally.
                    session_state::transitions::record_pending_guard_decision(
                        &mut self.state,
                        PendingGuardDecision {
                            stage: run.stage.clone(),
                            task_id: run.task_id,
                            round: run.round,
                            attempt: run.attempt,
                            run_id: run.id,
                            captured_head,
                            current_head,
                            warnings,
                        },
                    );
                    return Ok(None);
                }
            }
        }

        // No artifact (unknown stage) or artifact missing/invalid: exit code
        // takes precedence so the operator sees the real failure first.
        if let Some(code) = self.read_exit_status_code(run)
            && code != 0
        {
            if code > 128 {
                let signal_num = code - 128;
                let stamp_path = self.finish_stamp_path_for(run);
                let signal_received = crate::runner::read_finish_stamp(&stamp_path)
                    .map(|s| s.signal_received)
                    .unwrap_or_default();
                let operator_marker = Self::operator_termination_marker(&session_dir, run.id);
                // Reviewer note: legacy stamps also deserialize to an empty
                // signal marker, so this branch means the wrapper recorded no
                // trapped signal, not that we can distinguish historical gaps.
                let detail = if signal_received.is_empty() {
                    format!("agent exited {code}")
                } else {
                    format!("wrapper trapped {signal_received}")
                };
                let log_suffix = if signal_received.is_empty() && code == 129 {
                    " (agent CLI exited 129 on its own; wrapper trapped no signal)"
                } else {
                    ""
                };
                let _ = self.state.log_event(format!(
                    "run {} ({}) exited {code}: signal_received={signal_received}{log_suffix}",
                    run.id, run.stage
                ));
                if let Some(marker) = operator_marker {
                    return Ok(Some(match marker {
                        OperatorTerminationMarker::Stopped => "Operator Killed".to_string(),
                        OperatorTerminationMarker::RetryRequested => {
                            "user_forced_retry".to_string()
                        }
                    }));
                }
                return Ok(Some(format!("killed({signal_num}) [{detail}]")));
            }
            return Ok(Some(format!("exit({code})")));
        }

        // Guard reason beats artifact reason (coder control-file edits are a
        // real protocol violation; non-coder HEAD advances are hard errors).
        // PendingDecision here means artifact was missing/invalid — the run is
        // already a failure from the artifact check, so treat it as no guard error.
        let (mut guard_reason, guard_warnings) = match self.enforce_run_guard(run) {
            guard::VerifyResult::Ok { warnings } => (None, warnings),
            guard::VerifyResult::HardError { reason, warnings } => (Some(reason), warnings),
            guard::VerifyResult::PendingDecision { warnings, .. } => (None, warnings),
        };
        for w in guard_warnings {
            self.append_system_message(run.id, MessageKind::SummaryWarn, w);
        }
        if run.stage == "coder"
            && artifact_reason.is_none()
            && run.modes.yolo
            && guard_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("forbidden_control_edit"))
        {
            self.log_yolo_auto_approved("path_violation");
            guard_reason = None;
        }
        Ok(guard_reason.or(artifact_reason))
    }

    fn operator_termination_marker(
        session_dir: &std::path::Path,
        run_id: u64,
    ) -> Option<OperatorTerminationMarker> {
        #[derive(serde::Deserialize)]
        struct EventsFile {
            #[serde(default)]
            events: Vec<EventMessage>,
        }

        #[derive(serde::Deserialize)]
        struct EventMessage {
            message: String,
        }

        let Ok(events_text) = std::fs::read_to_string(session_dir.join("events.toml")) else {
            return None;
        };
        let Ok(events_file) = toml::from_str::<EventsFile>(&events_text) else {
            return None;
        };
        events_file
            .events
            .iter()
            .rev()
            .find_map(|event| match event.message.as_str() {
                marker if marker == format!("agent_stopped_by_user: run_id={run_id}") => {
                    Some(OperatorTerminationMarker::Stopped)
                }
                marker if marker == format!("agent_retry_requested_by_user: run_id={run_id}") => {
                    Some(OperatorTerminationMarker::RetryRequested)
                }
                marker if marker == format!("agent_killed_by_user: run_id={run_id}") => {
                    Some(OperatorTerminationMarker::RetryRequested)
                }
                _ => None,
            })
    }

    fn recovery_artifact_failure_reason(
        &mut self,
        recovery_path: &std::path::Path,
    ) -> Option<String> {
        let text = match std::fs::read_to_string(recovery_path) {
            Ok(text) => text,
            Err(err) => return Some(format!("artifact_invalid: {err}")),
        };
        let artifact: RecoveryArtifact = match toml::from_str(&text) {
            Ok(artifact) => artifact,
            Err(err) => return Some(format!("artifact_invalid: {err}")),
        };
        if artifact.summary.trim().is_empty() {
            return Some("artifact_invalid: recovery summary is empty".to_string());
        }
        if artifact.status != RecoveryStatus::Approved && artifact.feedback.is_empty() {
            return Some(format!(
                "artifact_invalid: recovery status={:?} requires at least one feedback item",
                artifact.status
            ));
        }

        let requested_trigger = match (artifact.status, artifact.trigger) {
            (RecoveryStatus::HumanBlocked, _) | (_, RecoveryStatus::HumanBlocked) => {
                Some("human_blocked")
            }
            (RecoveryStatus::AgentPivot, _) | (_, RecoveryStatus::AgentPivot) => {
                Some("agent_pivot")
            }
            _ => None,
        };
        if let Some(trigger) = requested_trigger {
            self.update_running_recovery_trigger(trigger);
        }

        match artifact.status {
            RecoveryStatus::Approved => None,
            RecoveryStatus::Revise => Some(format!(
                "recovery_requested_revise: {}",
                artifact.summary.trim()
            )),
            RecoveryStatus::HumanBlocked => Some(format!(
                "recovery_requested_human_blocked: {}",
                artifact.summary.trim()
            )),
            RecoveryStatus::AgentPivot => Some(format!(
                "recovery_requested_agent_pivot: {}",
                artifact.summary.trim()
            )),
        }
    }

    fn update_running_recovery_trigger(&mut self, trigger: &str) {
        let interactive = trigger == "human_blocked";
        let title = if interactive {
            "Human-blocked recovery"
        } else {
            "Agent pivot recovery"
        };
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|item| item.stage == "recovery" && item.status == PipelineItemStatus::Running)
        {
            item.trigger = Some(trigger.to_string());
            item.interactive = Some(interactive);
            item.title = Some(title.to_string());
        }
    }
}

