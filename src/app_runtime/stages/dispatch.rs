// Stage dispatch tables.
//
// Maps a `Phase`, `RetryLaunch` descriptor, `StageId`, or persisted run
// stage string to the corresponding per-stage launch entry point. Owning
// these tables here keeps stage orchestration in `app_runtime`: lifecycle,
// events, and finalization modules hand off through `app_runtime::stages`
// instead of matching on stage strings themselves.
use crate::{
    app::{App, AppStartupOrigin, RetryLaunch, StageId},
    lifecycle::{TickInput, TickOutcome},
    selection::CachedModel,
    state::Phase,
};
impl App {
    fn retry_gate_phase_for_descriptor(retry: &RetryLaunch) -> Phase {
        match retry {
            RetryLaunch::Brainstorm => Phase::BrainstormRunning,
            RetryLaunch::SpecReview => Phase::SpecReviewRunning,
            RetryLaunch::Planning => Phase::PlanningRunning,
            RetryLaunch::PlanReview => Phase::PlanReviewRunning,
            RetryLaunch::Sharding => Phase::ShardingRunning,
            RetryLaunch::Recovery => Phase::BuilderRecovery(1),
            RetryLaunch::RecoveryPlanReview => Phase::BuilderRecoveryPlanReview(1),
            RetryLaunch::RecoverySharding => Phase::BuilderRecoverySharding(1),
            RetryLaunch::Coder => Phase::ImplementationRound(1),
            RetryLaunch::Reviewer => Phase::ReviewRound(1),
            RetryLaunch::FinalValidation => Phase::FinalValidation(1),
            RetryLaunch::Dreaming => Phase::Dreaming(1),
        }
    }

    fn pause_sharding_retry_for_waiting_dispatch(&mut self) -> bool {
        self.clear_agent_error();
        self.current_run_id = None;
        self.run_launched = false;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.transition_to_phase(Phase::WaitingToImplement).is_ok()
    }

    /// Auto-launch the agent for the current phase if it's a non-interactive
    /// one. Idempotent: no-op if a run is already launched, if models aren't
    /// loaded, or if the last run errored (user needs to intervene).
    ///
    /// 5c routes the per-phase dispatch decision through
    /// [`crate::lifecycle::Scheduler::plan`]: this function builds a
    /// [`TickInput`] from the App's slim phase and FSM state, hands it to
    /// the scheduler, and dispatches the returned [`crate::lifecycle::StageSpec`]
    /// via [`Self::dispatch_start`]. The cross-session project-lane gate is
    /// enforced by the shell scheduler (see `app_shell::evaluate_tick`) so
    /// `project_lane_allows` is `true` here; the per-session lane gate inside
    /// [`Self::retry_allowed_by_project_lane`] still covers operator-initiated
    /// retries.
    pub(crate) fn maybe_auto_launch(&mut self) {
        if self.startup_origin == AppStartupOrigin::PickerCreated {
            return;
        }
        if self.run_launched || self.state.agent_error.is_some() {
            return;
        }
        if self.models.is_empty() {
            return;
        }
        // The shell scheduler owns the WaitingToImplement → Sharding /
        // RepoStateUpdate decision via `decide_waiting_dispatch`; per-session
        // auto-launch deliberately skips this phase so the baseline check
        // runs first. The slim phase for WaitingToImplement is `Phase::Plan`
        // and `Scheduler::plan` would otherwise pick Sharding here.
        if matches!(self.state.current_phase, Phase::WaitingToImplement) {
            return;
        }
        // DreamingPending is the operator-decision modal; the slim model
        // routes this through `PendingDecisions::blocks` once 5c-B / 5c-C
        // migrate the modal slots. Preserve the legacy no-op until then.
        if matches!(self.state.current_phase, Phase::DreamingPending) {
            return;
        }

        let spec = {
            let session_dir = self.session_dir();
            let session_id = self.state.session_id.clone();
            let prior_runs = self.slim_run_history();
            let stage_ctx = crate::lifecycle::StageCtx {
                session_id: session_id.as_str(),
                session_dir: session_dir.as_path(),
                phase: self.slim_phase,
                prior_runs: prior_runs.as_slice(),
                pending_task_ids: &[],
                yolo: self.state.modes.yolo,
                cheap: self.state.modes.cheap,
                recovery_active: matches!(
                    self.state.current_phase,
                    Phase::BuilderRecovery(_)
                        | Phase::BuilderRecoveryPlanReview(_)
                        | Phase::BuilderRecoverySharding(_)
                ),
                simplification_requested: matches!(
                    self.state.current_phase,
                    Phase::Simplification(_)
                ),
                dreaming_accepted: matches!(self.state.current_phase, Phase::Dreaming(_)),
            };
            let input = TickInput {
                agent: self.fsm.view(),
                phase: self.slim_phase,
                paused_at_phase: self.paused_at_phase,
                pending_decisions: &self.pending_decisions,
                project_lane_allows: true,
                ctx: stage_ctx,
            };
            match self.scheduler.plan(input) {
                TickOutcome::Dispatch(spec) => Some(spec),
                TickOutcome::Idle | TickOutcome::Blocked(_) => None,
            }
        };
        if let Some(spec) = spec {
            self.dispatch_start(&spec);
        }
    }
    /// Re-launch a stage after the operator stopped a running run with a
    /// retry intent. The descriptor was captured at modal-open time so
    /// transient state changes between modal-open and finalization don't
    /// mis-route the retry.
    pub(crate) fn launch_retry_from_descriptor(&mut self, retry: RetryLaunch) {
        let gate_phase = Self::retry_gate_phase_for_descriptor(&retry);
        if !self.retry_allowed_by_project_lane(gate_phase) {
            return;
        }
        match retry {
            RetryLaunch::Brainstorm => {
                let idea = self.state.idea_text.clone().unwrap_or_default();
                self.launch_brainstorm(idea);
            }
            RetryLaunch::SpecReview => self.launch_spec_review(),
            RetryLaunch::Planning => self.launch_planning(),
            RetryLaunch::PlanReview => self.launch_plan_review(),
            RetryLaunch::Sharding => {
                // A retry of sharding must land in WaitingToImplement so the
                // scheduler re-verifies the repo-state baseline before any
                // sharding launch — spec §Data model line 96.
                let _ = self.pause_sharding_retry_for_waiting_dispatch();
            }
            RetryLaunch::Recovery => self.launch_recovery(),
            RetryLaunch::RecoveryPlanReview => self.launch_recovery_plan_review(),
            RetryLaunch::RecoverySharding => self.launch_recovery_sharding(),
            RetryLaunch::Coder => self.launch_coder(),
            RetryLaunch::Reviewer => self.launch_reviewer(),
            RetryLaunch::FinalValidation => self.launch_final_validation(),
            RetryLaunch::Dreaming => self.launch_dreaming(),
        }
    }
    /// Launch the canonical retry for a `StageId` value carried by the
    /// stage-error modal. Cross-stage routing lives here so the modal
    /// handler only carries the keybinding contract.
    pub(crate) fn launch_retry_for_stage_id(&mut self, stage_id: StageId) {
        if !self.retry_allowed_by_project_lane(Self::retry_gate_phase_for_stage_id(stage_id)) {
            return;
        }
        match stage_id {
            StageId::Brainstorm => {
                let idea = self.state.idea_text.clone().unwrap_or_default();
                self.launch_brainstorm(idea);
            }
            StageId::SpecReview => self.launch_spec_review(),
            StageId::Planning => self.launch_planning(),
            StageId::PlanReview => self.launch_plan_review(),
            StageId::Sharding => {
                // Stage-error modal retry for sharding must route through
                // WaitingToImplement so the scheduler re-verifies baseline
                // state before any sharding launch — spec §Data model line 96.
                let _ = self.pause_sharding_retry_for_waiting_dispatch();
            }
            StageId::Implementation => self.launch_coder(),
            StageId::Review => self.launch_reviewer(),
            StageId::FinalValidation => self.launch_final_validation(),
            StageId::Dreaming => self.launch_dreaming(),
        }
    }
    pub(crate) fn launch_retry_for_stage(
        &mut self,
        failed_run: &crate::state::RunRecord,
        chosen: CachedModel,
    ) -> bool {
        if let Some(phase) = Self::retry_gate_phase_for_stage(&failed_run.stage)
            && !self.retry_allowed_by_project_lane(phase)
        {
            return false;
        }
        match failed_run.stage.as_str() {
            "brainstorm" => {
                let Some(idea) = self.state.idea_text.clone() else {
                    return false;
                };
                self.launch_brainstorm_with_model(idea, Some(chosen))
            }
            "spec-review" => self.launch_spec_review_with_model(Some(chosen)),
            "planning" => self.launch_planning_with_model(Some(chosen), true),
            "plan-review" => match self.state.current_phase {
                Phase::BuilderRecoveryPlanReview(_) => {
                    self.launch_recovery_plan_review_with_model(Some(chosen))
                }
                _ => self.launch_plan_review_with_model(Some(chosen)),
            },
            "sharding" => match self.state.current_phase {
                Phase::BuilderRecoverySharding(_) => {
                    self.launch_recovery_sharding_with_model(Some(chosen))
                }
                _ => self.pause_sharding_retry_for_waiting_dispatch(),
            },
            "recovery" => self.launch_recovery_with_model(Some(chosen)),
            "coder" => self.launch_coder_with_model(Some(chosen)),
            "reviewer" => self.launch_reviewer_with_model(Some(chosen)),
            "simplifier" => self.launch_simplifier_with_model(Some(chosen)),
            "final-validation" => self.launch_final_validation_with_model(Some(chosen)),
            "dreaming" => self.launch_dreaming_with_model(Some(chosen)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::StageId;
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::logic::selection::{
        CachedModel, Candidate, CliKind, IpbrPhaseScores, ScoreSource, SubscriptionKind,
    };
    use crate::state::{LaunchModes, Phase, RunRecord, RunStatus, SessionState};

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
            ipbr_phase_scores: IpbrPhaseScores {
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
            effort: crate::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn save_session(id: &str, phase: Phase) {
        let mut state = SessionState::new(id.to_string());
        state.idea_text = Some(format!("idea for {id}"));
        state.current_phase = phase;
        state.save().unwrap();
    }

    #[test]
    fn automatic_sharding_model_retry_pauses_in_waiting_to_implement() {
        // Automatic model fallback reaches this lower-level retry path, so
        // it must re-enter WaitingToImplement instead of launching sharding.
        with_temp_root(|| {
            let mut state = SessionState::new("20260511-093000-000000001".to_string());
            state.current_phase = Phase::ShardingRunning;
            let failed_run = failed_sharding_run();
            state.agent_runs.push(failed_run.clone());
            state.save().unwrap();

            let mut app = mk_app(state);
            app.models
                .push(cached_build_model(SubscriptionKind::Codex, "next-model"));

            assert!(app.launch_retry_for_stage(
                &failed_run,
                cached_build_model(SubscriptionKind::Codex, "next-model"),
            ));
            assert_eq!(app.state.current_phase, Phase::WaitingToImplement);
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
    fn focused_sharding_retry_is_rejected_when_another_session_occupies_impl_lane() {
        with_temp_root(|| {
            save_session("20260511-090000-000000001", Phase::ShardingRunning);
            let mut state = SessionState::new("20260511-091000-000000001".to_string());
            state.current_phase = Phase::BlockedNeedsUser;
            state.save().unwrap();

            let mut app = mk_app(state);
            app.launch_retry_for_stage_id(StageId::Sharding);

            assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        });
    }

    #[test]
    fn focused_later_phase_retries_are_rejected_when_another_session_occupies_impl_lane() {
        with_temp_root(|| {
            for (stage_id, phase) in [
                (StageId::Implementation, Phase::ImplementationRound(3)),
                (StageId::Review, Phase::ReviewRound(3)),
                (StageId::FinalValidation, Phase::FinalValidation(3)),
            ] {
                save_session("20260511-090000-000000001", Phase::ShardingRunning);
                let mut state = SessionState::new("20260511-091000-000000001".to_string());
                state.current_phase = phase;
                state.save().unwrap();

                let mut app = mk_app(state);
                app.launch_retry_for_stage_id(stage_id);

                assert_eq!(app.state.agent_runs.len(), 0, "stage: {stage_id:?}");
                assert!(
                    app.state.agent_error.is_none(),
                    "stage {stage_id:?} should be blocked before launch-specific validation"
                );
            }
        });
    }

    #[test]
    fn focused_planning_retry_is_allowed_while_another_session_occupies_impl_lane() {
        with_temp_root(|| {
            save_session("20260511-090000-000000001", Phase::ShardingRunning);
            let mut state = SessionState::new("20260511-091000-000000001".to_string());
            state.current_phase = Phase::PlanReviewPaused;
            state.save().unwrap();

            let mut app = mk_app(state);

            assert!(app.retry_allowed_by_project_lane(Phase::PlanningRunning));
        });
    }
}
