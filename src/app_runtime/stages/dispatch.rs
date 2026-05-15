// Per-tick scheduler dispatch entry point.
//
// The shell scheduler gates project-lane occupancy and then calls
// `maybe_auto_launch`, which asks `Scheduler::plan` for the next
// [`crate::lifecycle::StageSpec`] and hands it to [`App::dispatch_start`]
// for stage-specific launch wiring.
use crate::{
    app::{App, AppStartupOrigin, ModelRefreshState},
    lifecycle::{TickInput, TickOutcome},
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
        // The shell scheduler owns the WaitingToImplement → Sharding /
        // RepoStateUpdate decision via `decide_waiting_dispatch`; per-session
        // auto-launch deliberately skips this stage so the baseline check
        // runs first. The lifecycle stage for WaitingToImplement is `Stage::Plan`
        // and `Scheduler::plan` would otherwise pick Sharding here.
        if matches!(self.state.current_stage, Stage::WaitingToImplement) {
            return;
        }
        // DreamingPending is a persisted operator-decision stage, not a
        // PendingDecisions entry, so gate on the persisted stage.
        if matches!(self.state.current_stage, Stage::DreamingPending) {
            return;
        }

        let spec = self.with_lifecycle_stage_ctx(self.lifecycle_stage, |stage_ctx| {
            let input = TickInput {
                agent: self.fsm.view(),
                stage: self.lifecycle_stage,
                paused_at_stage: self.paused_at_stage,
                pending_decisions: &self.pending_decisions,
                project_lane_allows: true,
                ctx: stage_ctx,
            };
            match self.scheduler.plan(input) {
                TickOutcome::Dispatch(spec) => Some(spec),
                TickOutcome::Idle | TickOutcome::Blocked(_) => None,
            }
        });
        if let Some(spec) = spec {
            self.dispatch_start(&spec);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::logic::selection::{
        CachedModel, Candidate, CliKind, IpbrStageScores, ScoreSource, SubscriptionKind,
    };
    use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState, Stage};

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
}
