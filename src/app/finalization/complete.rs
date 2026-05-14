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
        match self.state.current_stage {
            Stage::BrainstormRunning => self.finalize_brainstorm_success(run)?,
            Stage::SpecReviewRunning => self.finalize_spec_review_success(run)?,
            Stage::PlanningRunning => self.finalize_planning_success(run)?,
            Stage::PlanReviewRunning => self.finalize_plan_review_success(run)?,
            Stage::RepoStateUpdateRunning => self.finalize_repo_state_update_success(run)?,
            Stage::ShardingRunning => self.finalize_sharding_success(run)?,
            Stage::ImplementationRound(round) => self.finalize_coder_success(run, round)?,
            Stage::ReviewRound(round) => self.finalize_reviewer_success(run, round)?,
            Stage::BuilderRecovery(round) => self.finalize_recovery_success(run, round)?,
            Stage::BuilderRecoveryPlanReview(round) => {
                self.handle_recovery_plan_review_completed(run, round)?
            }
            Stage::BuilderRecoverySharding(round) => {
                self.handle_recovery_sharding_completed(run, round)?
            }
            Stage::FinalValidation(round) => self.finalize_final_validation_success(run, round)?,
            Stage::Simplification(round) => self.finalize_simplification_success(run, round)?,
            Stage::Dreaming(round) => self.finalize_dreaming_success(run, round)?,
            Stage::IdeaInput
            | Stage::SpecReviewPaused
            | Stage::PlanReviewPaused
            | Stage::WaitingToImplement
            | Stage::BlockedNeedsUser
            | Stage::SkipToImplPending
            | Stage::GitGuardPending
            | Stage::DreamingPending
            | Stage::Done
            | Stage::Cancelled => {}
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
        if run.stage == "final-validation" {
            self.record_agent_error(error);
            self.transition_to_blocked(crate::state::BlockOrigin::FinalValidation)?;
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
