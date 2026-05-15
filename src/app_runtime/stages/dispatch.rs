// Per-tick scheduler dispatch entry point.
//
// The shell scheduler gates project-lane occupancy and then calls
// `maybe_auto_launch`, which asks `Scheduler::plan` for the next
// [`crate::lifecycle::StageSpec`] and hands it to [`App::dispatch_start`]
// for stage-specific launch wiring.
use crate::{
    app::{App, AppStartupOrigin, ModelRefreshState},
    lifecycle::StageId,
    state::Stage,
};
impl App {
    /// Auto-launch the agent for the current stage if it's a non-interactive
    /// one. Idempotent: no-op if a run is already launched, if models aren't
    /// loaded, or if the last run errored (user needs to intervene).
    ///
    /// Routes the per-stage dispatch decision through
    /// [`crate::lifecycle::Scheduler::plan`]: this function builds a
    /// [`TickInput`] from the App's lifecycle stage and FSM state, hands it to
    /// the scheduler, and dispatches the returned [`crate::lifecycle::StageSpec`]
    /// via [`Self::dispatch_start`]. The cross-session project-lane gate is
    /// enforced by the shell scheduler (see `app_shell::evaluate_tick`) so
    /// `project_lane_allows` is `true` here.
    pub(crate) fn maybe_auto_launch(&mut self) {
        if self.startup_origin == AppStartupOrigin::PickerCreated {
            return;
        }
        if self.run_launched || self.state.agent_error.is_some() {
            return;
        }
        if self.models.is_empty() {
            if matches!(self.model_refresh, ModelRefreshState::Idle(_)) {
                self.force_refresh_models();
            }
            return;
        }
        let stage_id = match self.state.current_stage {
            Stage::Idea => StageId::Brainstorm,
            Stage::Spec => StageId::SpecReview,
            Stage::Plan => StageId::Planning,
            Stage::BrainstormRunning => StageId::Brainstorm,
            Stage::SpecReviewRunning => StageId::SpecReview,
            Stage::PlanningRunning => StageId::Planning,
            Stage::PlanReviewRunning => StageId::PlanReview,
            Stage::RepoStateUpdateRunning => StageId::RepoStateUpdate,
            Stage::ShardingRunning => StageId::Sharding,
            Stage::Implementation(_) => StageId::Coder,
            Stage::BuilderRecovery(_) => StageId::Recovery,
            Stage::BuilderRecoveryPlanReview(_) => StageId::RecoveryPlanReview,
            Stage::BuilderRecoverySharding(_) => StageId::RecoverySharding,
            Stage::Review(_) => StageId::Reviewer,
            Stage::Simplification(_) => StageId::Simplification,
            Stage::Finalization => StageId::FinalValidation,
            Stage::FinalValidation(_) => StageId::FinalValidation,
            Stage::Dreaming(_) => StageId::Dreaming,
            // Operator/modal/queue states are intentionally idle here. In
            // particular, WaitingToImplement is driven by the shell-level
            // baseline gate, and PlanReviewPaused waits for the approval
            // modal instead of launching another review round.
            Stage::IdeaInput
            | Stage::SpecReviewPaused
            | Stage::PlanReviewPaused
            | Stage::WaitingToImplement
            | Stage::SkipToImplPending
            | Stage::GitGuardPending
            | Stage::DreamingPending
            | Stage::Blocked
            | Stage::BlockedNeedsUser
            | Stage::Done
            | Stage::Cancelled => return,
        };

        let spec = self.with_lifecycle_stage_ctx(self.lifecycle_stage, |stage_ctx| {
            if !matches!(self.fsm.view(), crate::lifecycle::AgentState::Idle) {
                return None;
            }
            if self.paused_at_stage == Some(self.lifecycle_stage) || self.pending_decisions.blocks()
            {
                return None;
            }
            let stage = self.scheduler.registry().get(stage_id)?;
            stage
                .next_pending_work(&stage_ctx)
                .map(|_| stage.build_spec(&stage_ctx))
        });
        if let Some(spec) = spec {
            self.dispatch_start(&spec);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::app::{TestLaunchHarness, TestLaunchOutcome};
    use crate::logic::selection::{
        CachedModel, Candidate, CliKind, IpbrStageScores, ScoreSource, SubscriptionKind,
    };
    use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState, Stage};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

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

    fn failed_sharding_run() -> RunRecord {
        RunRecord {
            id: 1,
            stage: "sharding".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "failed-model".to_string(),
            subscription_label: "claude".to_string(),
            window_name: "[Sharding] failed-model".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("model failed".to_string()),
            effort: crate::data::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn done_stage_run(id: u64, stage: &str, window_name: &str) -> RunRecord {
        RunRecord {
            id,
            stage: stage.to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "done-model".to_string(),
            subscription_label: "codex".to_string(),
            window_name: window_name.to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: crate::data::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn failed_plan_review_run(id: u64) -> RunRecord {
        let mut run = done_stage_run(id, "plan-review", "[Plan Review 1] failed-model");
        run.status = RunStatus::Failed;
        run.error = Some("aborted: TUI exited while running".to_string());
        run
    }

    fn write_spec_and_plan(session_id: &str) {
        let artifacts = crate::state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# plan\n").unwrap();
    }

    fn install_launch_harness(app: &mut crate::app::App, artifact_contents: Option<&str>) {
        app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
            outcomes: VecDeque::from([TestLaunchOutcome {
                exit_code: 0,
                artifact_contents: artifact_contents.map(str::to_string),
                launch_error: None,
            }]),
        })));
    }

    #[test]
    fn auto_launch_without_models_starts_model_refresh() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _guard = runtime.enter();
        let mut state = SessionState::new("20260511-093000-000000002".to_string());
        state.current_stage = Stage::BrainstormRunning;
        let mut app = mk_app(state);
        app.models.clear();
        app.run_launched = false;
        app.current_run_id = None;

        app.maybe_auto_launch();

        assert!(
            matches!(
                app.model_refresh,
                crate::app::ModelRefreshState::Fetching { .. }
            ),
            "auto-launch should start model refresh instead of staying idle with no candidates"
        );
    }

    #[test]
    fn model_guard_without_models_starts_refresh_without_agent_error() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _guard = runtime.enter();
        let mut state = SessionState::new("20260511-093000-000000003".to_string());
        state.current_stage = Stage::FinalValidation(1);
        let mut app = mk_app(state);
        app.models.clear();

        assert!(!app.guard_models_loaded());
        assert_eq!(app.state.agent_error, None);
        assert!(
            matches!(
                app.model_refresh,
                crate::app::ModelRefreshState::Fetching { .. }
            ),
            "missing models should kick a refresh instead of persisting a stage error"
        );
    }

    #[test]
    fn automatic_sharding_model_retry_pauses_in_waiting_to_implement() {
        // The model retry path re-enters WaitingToImplement so the shell
        // scheduler re-verifies the repo-state baseline before launching
        // sharding again.
        with_temp_root(|| {
            let mut state = SessionState::new("20260511-093000-000000001".to_string());
            state.current_stage = Stage::ShardingRunning;
            let failed_run = failed_sharding_run();
            state.agent_runs.push(failed_run.clone());
            state.save().unwrap();

            let mut app = mk_app(state);
            app.models
                .push(cached_build_model(SubscriptionKind::Codex, "next-model"));

            assert!(app.maybe_auto_retry(&failed_run));
            assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
            assert_eq!(
                app.state
                    .agent_runs
                    .iter()
                    .filter(|run| run.stage == "sharding")
                    .count(),
                1,
                "retry should not append a new sharding run before the waiting dispatch",
            );
        });
    }

    #[test]
    fn sharding_running_launches_sharding_not_plan_review_after_backfilled_review() {
        with_temp_root(|| {
            let session_id = "20260515-111500-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::ShardingRunning;
            state
                .agent_runs
                .push(done_stage_run(1, "planning", "[Planning] done-model"));
            state.agent_runs.push(failed_plan_review_run(2));
            state.save().unwrap();
            write_spec_and_plan(session_id);

            let mut app = mk_app(state);
            app.models
                .push(cached_build_model(SubscriptionKind::Codex, "next-model"));
            app.run_launched = false;
            app.current_run_id = None;
            install_launch_harness(&mut app, Some("[[tasks]]\nid = 1\ntitle = \"Implement\"\n"));

            app.maybe_auto_launch();

            let latest = app.state.agent_runs.last().expect("new run");
            assert_eq!(
                latest.stage, "sharding",
                "ShardingRunning must not be routed through lifecycle Plan candidates"
            );
        });
    }

    #[test]
    fn plan_review_paused_does_not_auto_launch_next_plan_review_round() {
        with_temp_root(|| {
            let mut state = SessionState::new("20260515-111500-000000002".to_string());
            state.current_stage = Stage::PlanReviewPaused;
            state
                .agent_runs
                .push(done_stage_run(1, "planning", "[Planning] done-model"));
            state.agent_runs.push(done_stage_run(
                2,
                "plan-review",
                "[Plan Review 1] done-model",
            ));
            state.save().unwrap();
            let original_run_count = state.agent_runs.len();

            let mut app = mk_app(state);
            app.models
                .push(cached_build_model(SubscriptionKind::Codex, "next-model"));
            app.run_launched = false;
            app.current_run_id = None;
            install_launch_harness(&mut app, Some("# review\n"));

            app.maybe_auto_launch();

            assert_eq!(
                app.state.agent_runs.len(),
                original_run_count,
                "paused plan review should wait for the operator modal"
            );
        });
    }
}
