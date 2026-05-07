// Stage dispatch tables.
//
// Maps a `Phase`, `RetryLaunch` descriptor, `StageId`, or persisted run
// stage string to the corresponding per-stage launch entry point. Owning
// these tables here is what makes `src/app/` a compatibility shim rather
// than the home of stage orchestration: the legacy lifecycle / events /
// finalization modules now hand off through `app_runtime::stages` instead
// of matching on stage strings themselves.
use crate::{
    app::{App, AppStartupOrigin, RetryLaunch, StageId},
    selection::CachedModel,
    state::Phase,
};
impl App {
    /// Auto-launch the agent for the current phase if it's a non-interactive
    /// one (spec review, sharding, coder, reviewer). Idempotent: no-op if the
    /// run is already launched, if models aren't loaded, or if the last run
    /// errored (user needs to intervene).
    pub(crate) fn maybe_auto_launch(&mut self) {
        if self.startup_origin == AppStartupOrigin::PickerCreated {
            return;
        }
        if self.run_launched || self.state.agent_error.is_some() || self.models.is_empty() {
            return;
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                if let Some(idea) = self.state.idea_text.clone() {
                    self.launch_brainstorm(idea);
                }
            }
            Phase::SpecReviewRunning => self.launch_spec_review(),
            Phase::PlanningRunning => self.launch_planning(),
            Phase::PlanReviewRunning => self.launch_plan_review(),
            Phase::ShardingRunning => self.launch_sharding(),
            Phase::ImplementationRound(_) => self.launch_coder(),
            Phase::ReviewRound(_) => self.launch_reviewer(),
            Phase::BuilderRecovery(_) => self.launch_recovery(),
            Phase::BuilderRecoveryPlanReview(_) => self.launch_recovery_plan_review(),
            Phase::BuilderRecoverySharding(_) => self.launch_recovery_sharding(),
            Phase::Simplification(_) => self.launch_simplifier(),
            Phase::FinalValidation(_) => self.launch_final_validation(),
            Phase::Dreaming(_) => self.launch_dreaming(),
            Phase::DreamingPending => {}
            _ => {}
        }
    }
    /// Re-launch a stage after the operator stopped a running run with a
    /// retry intent. The descriptor was captured at modal-open time so
    /// transient state changes between modal-open and finalization don't
    /// mis-route the retry.
    pub(crate) fn launch_retry_from_descriptor(&mut self, retry: RetryLaunch) {
        match retry {
            RetryLaunch::Brainstorm => {
                let idea = self.state.idea_text.clone().unwrap_or_default();
                self.launch_brainstorm(idea);
            }
            RetryLaunch::SpecReview => self.launch_spec_review(),
            RetryLaunch::Planning => self.launch_planning(),
            RetryLaunch::PlanReview => self.launch_plan_review(),
            RetryLaunch::Sharding => self.launch_sharding(),
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
        match stage_id {
            StageId::Brainstorm => {
                let idea = self.state.idea_text.clone().unwrap_or_default();
                self.launch_brainstorm(idea);
            }
            StageId::SpecReview => self.launch_spec_review(),
            StageId::Planning => self.launch_planning(),
            StageId::PlanReview => self.launch_plan_review(),
            StageId::Sharding => self.launch_sharding(),
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
                _ => self.launch_sharding_with_model(Some(chosen)),
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
