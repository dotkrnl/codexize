use crate::app::{App, TerminationIntent};
use crate::state::Phase;
use anyhow::Result;
impl App {
    pub(crate) fn complete_run_finalization(
        &mut self,
        run: &crate::state::RunRecord,
        failure_reason: Option<String>,
    ) -> Result<()> {
        if let Some(error) = failure_reason {
            return self.handle_run_finalization_failure(run, error);
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => self.finalize_brainstorm_success(run)?,
            Phase::SpecReviewRunning => self.finalize_spec_review_success(run)?,
            Phase::PlanningRunning => self.finalize_planning_success(run)?,
            Phase::PlanReviewRunning => self.finalize_plan_review_success(run)?,
            Phase::ShardingRunning => self.finalize_sharding_success(run)?,
            Phase::ImplementationRound(round) => self.finalize_coder_success(run, round)?,
            Phase::ReviewRound(round) => self.finalize_reviewer_success(run, round)?,
            Phase::BuilderRecovery(round) => self.finalize_recovery_success(run, round)?,
            Phase::BuilderRecoveryPlanReview(round) => {
                self.handle_recovery_plan_review_completed(run, round)?
            }
            Phase::BuilderRecoverySharding(round) => {
                self.handle_recovery_sharding_completed(run, round)?
            }
            Phase::FinalValidation(round) => self.finalize_final_validation_success(run, round)?,
            Phase::Simplification(round) => self.finalize_simplification_success(run, round)?,
            Phase::IdeaInput
            | Phase::SpecReviewPaused
            | Phase::PlanReviewPaused
            | Phase::BlockedNeedsUser
            | Phase::SkipToImplPending
            | Phase::GitGuardPending
            | Phase::Done => {}
        }
        Ok(())
    }
    fn handle_run_finalization_failure(
        &mut self,
        run: &crate::state::RunRecord,
        error: String,
    ) -> Result<()> {
        self.finalize_run_record(run.id, false, Some(error.clone()));
        let pending_termination = self
            .pending_termination
            .as_ref()
            .filter(|pending| pending.run_id == run.id)
            .cloned();
        if let Some(pending) = pending_termination {
            self.pending_termination = None;
            self.clear_agent_error();
            match pending.intent {
                TerminationIntent::StopOnly => {}
                TerminationIntent::StopAndRetry(retry) => {
                    self.launch_retry_from_descriptor(retry);
                }
                TerminationIntent::StopAndQuit => {
                    self.pending_app_exit = true;
                }
            }
            return Ok(());
        }
        if matches!(error.as_str(), "Operator Killed" | "user_forced_retry") {
            self.clear_agent_error();
            return Ok(());
        }
        if run.stage == "final-validation" {
            self.record_agent_error(error);
            self.transition_to_blocked(crate::state::BlockOrigin::FinalValidation)?;
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
}
