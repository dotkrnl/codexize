use crate::app::App;
use crate::state::Stage;
use anyhow::Result;
impl App {
    pub(crate) fn complete_run_finalization(
        &mut self,
        run: &crate::state::RunRecord,
        failure_reason: Option<String>,
    ) -> Result<()> {
        // The operator's pending command (`:cancel`) leaves the FSM in
        // `Stopping { after: Cancel }` until `confirm_dead` lands inside
        // `finalize_run_record`. Read it here so cancel terminates to
        // `Stage::Cancelled` exactly like the persisted `pending_termination`
        // mirror used to.
        let fsm_cancellation = matches!(
            self.fsm.view(),
            crate::lifecycle::AgentState::Stopping {
                after: crate::lifecycle::AfterStop::Cancel,
                ..
            }
        );
        if fsm_cancellation {
            self.clear_agent_error();
            self.finalize_run_record(run.id, failure_reason.is_none(), failure_reason);
            self.transition_to_stage(Stage::Cancelled)?;
            return Ok(());
        }
        if let Some(error) = failure_reason {
            return self.handle_run_finalization_failure(run, error);
        }
        let stage_id = crate::lifecycle::stage_id_for_run(&run.stage, &run.window_name);
        match stage_id {
            Some(crate::lifecycle::StageId::Brainstorm) => self.finalize_brainstorm_success(run)?,
            Some(crate::lifecycle::StageId::SpecReview) => {
                self.finalize_spec_review_success(run)?
            }
            Some(crate::lifecycle::StageId::Planning) => self.finalize_planning_success(run)?,
            Some(crate::lifecycle::StageId::PlanReview) => {
                self.finalize_plan_review_success(run)?
            }
            Some(crate::lifecycle::StageId::RepoStateUpdate) => {
                self.finalize_repo_state_update_success(run)?
            }
            Some(crate::lifecycle::StageId::Sharding) => {
                if matches!(self.state.current_stage, Stage::BuilderRecoverySharding(_)) {
                    self.handle_recovery_sharding_completed(run, run.round)?;
                } else {
                    self.finalize_sharding_success(run)?;
                }
            }
            Some(crate::lifecycle::StageId::Coder) => {
                self.finalize_coder_success(run, run.round)?
            }
            Some(crate::lifecycle::StageId::Reviewer) => {
                self.finalize_reviewer_success(run, run.round)?
            }
            Some(crate::lifecycle::StageId::Recovery) => {
                self.finalize_recovery_success(run, run.round)?
            }
            Some(crate::lifecycle::StageId::RecoveryPlanReview) => {
                self.handle_recovery_plan_review_completed(run, run.round)?;
            }
            Some(crate::lifecycle::StageId::RecoverySharding) => {
                self.handle_recovery_sharding_completed(run, run.round)?;
            }
            Some(crate::lifecycle::StageId::FinalValidation) => {
                self.finalize_final_validation_success(run, run.round)?
            }
            Some(crate::lifecycle::StageId::Simplification) => {
                self.finalize_simplification_success(run, run.round)?
            }
            Some(crate::lifecycle::StageId::Dreaming) => {
                self.finalize_dreaming_success(run, run.round)?
            }
            None => {}
        }
        Ok(())
    }
    fn handle_run_finalization_failure(
        &mut self,
        run: &crate::state::RunRecord,
        error: String,
    ) -> Result<()> {
        // Capture the FSM's pending stop intent *before* `finalize_run_record`
        // calls `confirm_dead` — that call resolves the FSM back toward Idle,
        // so we'd lose the operator's `:stop` / `:retry` / `:cancel` request
        // if we read the state afterwards.
        let pending_after_stop = self.snapshot_pending_after_stop();
        self.finalize_run_record(run.id, false, Some(error.clone()));
        match pending_after_stop {
            Some(PendingAfterStop::GoIdle) => {
                self.clear_agent_error();
                return Ok(());
            }
            Some(PendingAfterStop::Restart) => {
                // Operator-initiated `:retry`/restart after a `:stop`. The
                // failure-finalize path normally leaves `current_stage` on
                // the failed stage's running variant, which the scheduler
                // tick already routes back to the same stage. Clear the
                // error so the auto-launch guard doesn't short-circuit and
                // drive a tick directly. The shell loop will tick again, but
                // firing eagerly here keeps perceived latency low.
                self.clear_agent_error();
                self.maybe_auto_launch();
                return Ok(());
            }
            Some(PendingAfterStop::Cancel) => {
                self.clear_agent_error();
                self.transition_to_stage(Stage::Cancelled)?;
                return Ok(());
            }
            None => {}
        }
        if matches!(error.as_str(), "Operator Killed" | "user_forced_retry") {
            self.clear_agent_error();
            return Ok(());
        }
        if run.stage == "dreaming" {
            self.record_agent_error(error);
            return Ok(());
        }
        let failed_run = self
            .state
            .agent_runs
            .iter()
            .find(|candidate| candidate.id == run.id)
            .cloned()
            .unwrap_or_else(|| run.clone());
        if !self.maybe_auto_retry(&failed_run) {
            self.record_agent_error(error);
        }
        Ok(())
    }

    /// Read the operator's pending stop intent out of the lifecycle FSM
    /// (or its mirror). Returns `None` when the FSM is not in `Stopping`,
    /// when the stop was `Rewind` (handled separately via
    /// `apply_after_stop_rewind`), or when the stop was a runner-side
    /// failure with no operator intent attached.
    fn snapshot_pending_after_stop(&self) -> Option<PendingAfterStop> {
        match self.fsm.view() {
            crate::lifecycle::AgentState::Stopping { after, .. } => match after {
                crate::lifecycle::AfterStop::GoIdle => Some(PendingAfterStop::GoIdle),
                crate::lifecycle::AfterStop::Restart { .. } => Some(PendingAfterStop::Restart),
                crate::lifecycle::AfterStop::Cancel => Some(PendingAfterStop::Cancel),
                // Rewind has its own resolution path inside finalize_run_record.
                crate::lifecycle::AfterStop::Rewind { .. } => None,
            },
            _ => None,
        }
    }
}

/// Operator stop intent surfaced to the failure-handling path. Mirrors a
/// subset of [`crate::lifecycle::AfterStop`]; `Rewind` is handled
/// separately by `apply_after_stop_rewind`.
enum PendingAfterStop {
    GoIdle,
    Restart,
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::app::{TestLaunchHarness, TestLaunchOutcome};
    use crate::data::adapters::EffortLevel;
    use crate::logic::selection::{
        CachedModel, Candidate, CliKind, IpbrStageScores, ScoreSource, SubscriptionKind,
    };
    use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn run_record(id: u64, stage: &str, window_name: &str) -> RunRecord {
        RunRecord {
            id,
            stage: stage.to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            subscription_label: "test-vendor".to_string(),
            window_name: window_name.to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes {
                interactive: true,
                ..LaunchModes::default()
            },
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn cached_build_model(vendor: SubscriptionKind, name: &str) -> CachedModel {
        let candidate = Candidate {
            subscription: vendor,
            cli: vendor.direct_cli().unwrap_or(CliKind::Codex),
            launch_name: name.to_string(),
            quota_percent: Some(80),
            quota_resets_at: None,
            display_order: 0,
            enabled: true,
            free: false,
            official: true,
            quota_disabled: false,
            cheap_eligible: true,
            tough_eligible: true,
            effort_eligible: true,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            quota_failed: false,
        };
        CachedModel {
            subscription: vendor,
            name: name.to_string(),
            ipbr_stage_scores: IpbrStageScores {
                idea: Some(80.0),
                planning: Some(80.0),
                build: Some(80.0),
                review: Some(80.0),
            },
            score_source: ScoreSource::Ipbr,
            candidates: vec![candidate],
            selected_candidate: Some(0),
            quota_percent: Some(80),
            quota_resets_at: None,
            display_order: 0,
        }
    }

    fn write_final_validation_inputs(session_id: &str) {
        let artifacts = crate::state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# spec\n").unwrap();
    }

    #[test]
    fn plan_review_run_finishing_after_waiting_dispatch_is_not_finalized_as_sharding() {
        with_temp_root(|| {
            let session_id = "20260515-101500-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::ShardingRunning;
            let run = run_record(7, "plan-review", "[Plan Review 1] test-model");
            state.agent_runs.push(run.clone());
            state.save().unwrap();

            let mut app = mk_app(state);
            app.complete_run_finalization(&run, None)
                .expect("plan-review completion must not require tasks.toml");

            let finished = app.state.agent_runs.iter().find(|r| r.id == 7).unwrap();
            assert_eq!(finished.status, RunStatus::Done);
            assert_eq!(app.state.current_stage, Stage::ShardingRunning);
        });
    }

    #[test]
    fn planning_run_finishing_after_stage_advance_is_not_finalized_as_plan_review() {
        with_temp_root(|| {
            let session_id = "20260515-101500-000000002";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::PlanReviewRunning;
            let run = run_record(8, "planning", "[Planning] test-model");
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            let artifacts = crate::state::session_dir(session_id).join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(artifacts.join("plan.md"), "# plan\n").unwrap();

            let mut app = mk_app(state);
            app.complete_run_finalization(&run, None)
                .expect("planning completion should keep the already-advanced stage");

            let finished = app.state.agent_runs.iter().find(|r| r.id == 8).unwrap();
            assert_eq!(finished.status, RunStatus::Done);
            assert_eq!(app.state.current_stage, Stage::PlanReviewRunning);
        });
    }

    #[test]
    fn failed_final_validation_auto_retries_instead_of_blocking_immediately() {
        with_temp_root(|| {
            let session_id = "20260515-101500-000000003";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::FinalValidation(1);
            let mut run = run_record(9, "final-validation", "[FinalValidation] failed-model");
            run.model = "failed-model".to_string();
            run.subscription_label = "claude".to_string();
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            write_final_validation_inputs(session_id);

            let mut app = mk_app(state);
            app.models = vec![
                cached_build_model(SubscriptionKind::Claude, "failed-model"),
                cached_build_model(SubscriptionKind::Codex, "retry-model"),
            ];
            app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            })));
            app.run_launched = false;
            app.current_run_id = None;

            app.complete_run_finalization(&run, Some("artifact validation failed".to_string()))
                .expect("finalization failure should be handled");

            let failed = app.state.agent_runs.iter().find(|r| r.id == 9).unwrap();
            assert_eq!(failed.status, RunStatus::Failed);
            assert_eq!(app.state.current_stage, Stage::FinalValidation(1));
            assert_eq!(app.state.agent_error, None);
            assert_eq!(app.state.block_origin, None);
            let retry = app.state.agent_runs.last().unwrap();
            assert_eq!(retry.stage, "final-validation");
            assert_eq!(retry.status, RunStatus::Running);
            assert_eq!(retry.attempt, 2);
            assert_eq!(retry.model, "retry-model");
        });
    }
}
