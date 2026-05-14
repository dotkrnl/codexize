use super::super::run_helpers::OperatorTerminationMarker;
use super::Reason;
use crate::app::prompts::{read_review_scope_base_sha, validate_stage_toml_writes};
use crate::app::{App, guard};
use crate::data::artifacts::{RecoveryArtifact, ReviewStatus as RecoveryStatus};
use crate::state::{self as session_state, MessageKind, PendingGuardDecision, PipelineItemStatus};
use crate::{coder_summary, data::validation as final_validation, review, tasks};
impl App {
    const ARTIFACT_REASON_TABLE: &[(&'static str, &'static [&'static str])] = &[
        ("brainstorm", &["artifacts/spec.md"]),
        ("spec-review", &["artifacts/spec-review-{round}.md"]),
        ("planning", &["artifacts/plan.md"]),
        ("plan-review", &["artifacts/plan-review-{round}.md"]),
        ("sharding", &["artifacts/tasks.toml"]),
        (
            "recovery",
            &[
                "artifacts/spec.md",
                "artifacts/plan.md",
                "artifacts/tasks.toml",
                "rounds/{round:03}/recovery.toml",
            ],
        ),
        ("reviewer", &["rounds/{round:03}/review.toml"]),
        (
            "final-validation",
            &["artifacts/final_validation_{round}.toml"],
        ),
        ("simplifier", &["rounds/{round:03}/simplification.toml"]),
    ];
    fn render_artifact_template(template: &str, round: u32) -> String {
        template
            .replace("{round:03}", &format!("{round:03}"))
            .replace("{round}", &round.to_string())
    }
    fn round_dir(session_dir: &std::path::Path, round: u32) -> std::path::PathBuf {
        session_dir.join("rounds").join(format!("{:03}", round))
    }
    fn invalid_artifact(err: impl std::fmt::Display) -> String {
        Reason::ArtifactInvalid(err.to_string()).to_string()
    }
    fn missing_artifact_reasons(
        session_dir: &std::path::Path,
        stage: &str,
        round: u32,
    ) -> Option<String> {
        let (_, templates) = Self::ARTIFACT_REASON_TABLE
            .iter()
            .find(|(c, _)| *c == stage)?;
        templates
            .iter()
            .map(|template| session_dir.join(Self::render_artifact_template(template, round)))
            .any(|path| !Self::artifact_present(&path))
            .then(|| Reason::ArtifactMissing.to_string())
    }
    pub(crate) fn failed_unverified_reason(
        stamp_path: &std::path::Path,
        detail: impl AsRef<str>,
    ) -> String {
        Reason::FailedUnverified {
            detail: detail.as_ref().to_string(),
            path: stamp_path.display().to_string(),
        }
        .to_string()
    }
    pub(crate) fn coder_gate_reason(
        &self,
        run: &crate::state::RunRecord,
        round_dir: &std::path::Path,
    ) -> Option<String> {
        let scope_file = round_dir.join("review_scope.toml");
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return (!Self::artifact_present(&scope_file)).then(|| Reason::BaseMissing.to_string());
        }
        if !Self::artifact_present(&scope_file) {
            return Some(Reason::BaseMissing.to_string());
        }
        let Ok(base) = read_review_scope_base_sha(&scope_file) else {
            return Some(Reason::BaseMissing.to_string());
        };
        if base.is_empty() {
            return Some(Reason::BaseMissing.to_string());
        }
        let stamp_path = self.finish_stamp_path_for(run);
        if !Self::artifact_present(&stamp_path) {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "missing finish stamp",
            ));
        }
        let Ok(stamp) = crate::data::runner::read_finish_stamp(&stamp_path) else {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "malformed finish stamp",
            ));
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
            let Ok(summary) = coder_summary::validate(&summary_path) else {
                return Some(Reason::InvalidCoderSummary.to_string());
            };
            return match summary.status {
                coder_summary::CoderStatus::Done => None,
                coder_summary::CoderStatus::Partial => Some(Reason::CoderPartial.to_string()),
            };
        }
        if stamp.head_after == base {
            return Some(Reason::MissingCoderSummary.to_string());
        }
        None
    }
    pub(crate) fn simplifier_dirty_tree_reason(
        &self,
        run: &crate::state::RunRecord,
    ) -> Option<String> {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return None;
        }
        let stamp_path = self.finish_stamp_path_for(run);
        let Ok(stamp) = crate::data::runner::read_finish_stamp(&stamp_path) else {
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
    pub(crate) fn normalized_failure_reason(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Option<String> {
        let session_dir = self.session_dir();
        let (has_artifact_check, artifact_reason) = match run.stage.as_str() {
            "brainstorm" | "spec-review" | "planning" | "plan-review" => (
                true,
                Self::missing_artifact_reasons(&session_dir, &run.stage, run.round),
            ),
            "sharding" => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let reason = Self::missing_artifact_reasons(&session_dir, "sharding", run.round)
                    .or_else(|| {
                        tasks::validate(&tasks_path)
                            .err()
                            .map(Self::invalid_artifact)
                    });
                (true, reason)
            }
            "recovery" => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let reason = Self::missing_artifact_reasons(&session_dir, "recovery", run.round)
                    .or_else(|| {
                        validate_stage_toml_writes(&session_dir, "recovery", run.round)
                            .err()
                            .map(Self::invalid_artifact)
                    })
                    .or_else(|| {
                        let recovery_path =
                            Self::round_dir(&session_dir, run.round).join("recovery.toml");
                        match tasks::validate(&tasks_path) {
                            Ok(_) => self.recovery_artifact_failure_reason(&recovery_path),
                            Err(err) => Some(Self::invalid_artifact(err)),
                        }
                    });
                (true, reason)
            }
            "coder" => {
                let round_dir = Self::round_dir(&session_dir, run.round);
                (false, self.coder_gate_reason(run, &round_dir))
            }
            "reviewer" => {
                let review_path = Self::round_dir(&session_dir, run.round).join("review.toml");
                let reason = Self::missing_artifact_reasons(&session_dir, "reviewer", run.round)
                    .or_else(|| {
                        review::validate(&review_path)
                            .err()
                            .map(Self::invalid_artifact)
                    });
                (true, reason)
            }
            "final-validation" => {
                let verdict_path = session_dir
                    .join("artifacts")
                    .join(format!("final_validation_{}.toml", run.round));
                let reason =
                    Self::missing_artifact_reasons(&session_dir, "final-validation", run.round)
                        .or_else(|| {
                            final_validation::validate(&verdict_path)
                                .err()
                                .map(Self::invalid_artifact)
                        });
                (true, reason)
            }
            "dreaming" => {
                let report_path =
                    crate::logic::memory::dream_report_path(&self.memory_root(), run.round);
                let reason = (!Self::artifact_present(&report_path))
                    .then(|| Reason::ArtifactMissing.to_string())
                    .or_else(|| {
                        crate::data::memory::validate_dream_report_file(&report_path)
                            .err()
                            .map(Self::invalid_artifact)
                    });
                (true, reason)
            }
            "simplifier" => {
                let simplification_path =
                    Self::round_dir(&session_dir, run.round).join("simplification.toml");
                let reason = Self::missing_artifact_reasons(&session_dir, "simplifier", run.round)
                    .or_else(|| {
                        crate::simplification::validate(&simplification_path)
                            .err()
                            .map(Self::invalid_artifact)
                    })
                    .or_else(|| self.simplifier_dirty_tree_reason(run));
                (true, reason)
            }
            _ => (false, None),
        };
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
                    return None;
                }
                guard::VerifyResult::HardError { reason, warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Some(reason);
                }
                guard::VerifyResult::PendingDecision {
                    captured_head,
                    current_head,
                    warnings,
                } => {
                    session_state::record_pending_guard_decision(
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
                    return None;
                }
            }
        }
        if let Some(code) = self.read_exit_status_code(run)
            && code != 0
        {
            if code > 128 {
                let signal_num = code - 128;
                let stamp_path = self.finish_stamp_path_for(run);
                let signal_received = crate::data::runner::read_finish_stamp(&stamp_path)
                    .map(|s| s.signal_received)
                    .unwrap_or_default();
                let operator_marker = Self::operator_termination_marker(&session_dir, run.id);
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
                return Some(match operator_marker {
                    Some(OperatorTerminationMarker::Stopped) => Reason::OperatorKilled.to_string(),
                    Some(OperatorTerminationMarker::RetryRequested) => {
                        Reason::UserForcedRetry.to_string()
                    }
                    None => Reason::Killed { signal_num, detail }.to_string(),
                });
            }
            return Some(Reason::ExitCode(code).to_string());
        }
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
        guard_reason.or(artifact_reason)
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
            Err(err) => return Some(Self::invalid_artifact(err)),
        };
        let artifact: RecoveryArtifact = match toml::from_str(&text) {
            Ok(artifact) => artifact,
            Err(err) => return Some(Self::invalid_artifact(err)),
        };
        if artifact.summary.trim().is_empty() {
            return Some(Reason::RecoverySummaryEmpty.to_string());
        }
        if artifact.status != RecoveryStatus::Approved && artifact.feedback.is_empty() {
            return Some(
                Reason::RecoveryMissingFeedback(format!("{:?}", artifact.status)).to_string(),
            );
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
        let summary = artifact.summary.trim().to_string();
        match artifact.status {
            RecoveryStatus::Approved => None,
            RecoveryStatus::Revise => Some(Reason::RecoveryRequestedRevise(summary).to_string()),
            RecoveryStatus::HumanBlocked => {
                Some(Reason::RecoveryRequestedHumanBlocked(summary).to_string())
            }
            RecoveryStatus::AgentPivot => {
                Some(Reason::RecoveryRequestedAgentPivot(summary).to_string())
            }
        }
    }
    fn update_running_recovery_trigger(&mut self, trigger: &str) {
        let interactive = trigger == "human_blocked";
        let title = if interactive {
            "Human-blocked recovery"
        } else {
            "Agent pivot recovery"
        };
        let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|item| item.stage == "recovery" && item.status == PipelineItemStatus::Running)
        else {
            return;
        };
        item.trigger = Some(trigger.to_string());
        item.interactive = Some(interactive);
        item.title = Some(title.to_string());
    }
}
