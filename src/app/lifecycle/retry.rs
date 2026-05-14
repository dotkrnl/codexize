use super::{
    RetryTarget, Severity, parse_task_label_id, retry_stage_for_label, retry_target_for_run,
};
use crate::app::App;
use crate::app::tree::node_at_path;
use crate::lifecycle::{LifecycleOps, Phase as SlimPhase};
use crate::state::{self as session_state, NodeKind, Phase};
use std::time::Duration;

/// Map the operator-visible [`crate::app::StageId`] modal target to
/// the lifecycle-internal [`crate::lifecycle::StageId`] used by the stage
/// registry. These values are intentionally stage-error targets, not slim
/// phases: multiple concrete lifecycle stages share a phase, but a retry
/// from the modal must relaunch the stage that actually failed.
fn lifecycle_stage_id_from_view(view: crate::app::StageId) -> crate::lifecycle::StageId {
    use crate::app::StageId as V;
    use crate::lifecycle::StageId as L;
    match view {
        V::Brainstorm => L::Brainstorm,
        V::SpecReview => L::SpecReview,
        V::Planning => L::Planning,
        V::PlanReview => L::PlanReview,
        V::RepoStateUpdate => L::RepoStateUpdate,
        V::Sharding => L::Sharding,
        V::Implementation => L::Coder,
        V::Recovery => L::Recovery,
        V::RecoveryPlanReview => L::RecoveryPlanReview,
        V::RecoverySharding => L::RecoverySharding,
        V::Review => L::Reviewer,
        V::Simplification => L::Simplification,
        V::FinalValidation => L::FinalValidation,
        V::Dreaming => L::Dreaming,
    }
}
impl App {
    /// Relaunch a specific failed stage from the stage-error modal.
    ///
    /// The modal's "retry" button hands us a [`crate::app::StageId`]
    /// (the operator-visible category, 9 variants). We project it down to
    /// the lifecycle stage, build a fresh [`crate::lifecycle::StageSpec`]
    /// from the stage's `build_spec`, and dispatch via
    /// [`App::dispatch_start`]. Going through `dispatch_start` (rather
    /// than the scheduler tick) keeps the modal's stage choice
    /// authoritative — slim `Phase::Finalization` covers both
    /// FinalValidation and Dreaming, so `Scheduler::plan` would otherwise
    /// pick FinalValidation first even when the operator clicked "retry"
    /// on the dreaming modal.
    pub(crate) fn retry_failed_stage(&mut self, view_stage_id: crate::app::StageId) {
        let stage_id = lifecycle_stage_id_from_view(view_stage_id);
        let spec = self.with_lifecycle_stage_ctx(self.slim_phase, |ctx| {
            self.scheduler
                .registry()
                .get(stage_id)
                .map(|stage| stage.build_spec(&ctx))
        });
        // clear_agent_error so the maybe_auto_launch guard inside
        // start_run_tracking can release the run; without this the
        // dispatched launch would silently no-op.
        self.clear_agent_error();
        if let Some(spec) = spec {
            self.dispatch_start(&spec);
        }
    }

    pub(crate) fn selected_retry_target(&self) -> Option<RetryTarget> {
        let row = self.visible_rows.get(self.selected)?;
        for depth in (1..=row.path.len()).rev() {
            let node = node_at_path(&self.nodes, &row.path[..depth])?;
            if node.kind == NodeKind::Task {
                return parse_task_label_id(&node.label).map(RetryTarget::Task);
            }
            if node.kind == NodeKind::Stage
                && let Some(stage) = retry_stage_for_label(&node.label)
            {
                return Some(RetryTarget::Stage(stage));
            }
        }
        row.backing_leaf_run_id
            .and_then(|run_id| {
                self.state
                    .agent_runs
                    .iter()
                    .find(|run| run.id == run_id)
                    .and_then(retry_target_for_run)
            })
            .or_else(|| {
                self.current_run_id.and_then(|run_id| {
                    self.state
                        .agent_runs
                        .iter()
                        .find(|run| run.id == run_id)
                        .and_then(retry_target_for_run)
                })
            })
            .or_else(|| self.state.builder.current_task_id().map(RetryTarget::Task))
    }
    pub(crate) fn retry_selected_target(&mut self) {
        let Some(target) = self.selected_retry_target() else {
            self.push_status(
                "rewind: select a stage or task first".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        };
        let target_phase = match target {
            RetryTarget::Task(task_id) => slim_phase_for_task_retry(task_id, &self.state),
            RetryTarget::Stage(stage) => slim_phase_for_stage_retry(stage),
        };
        self.run_lifecycle_op("retry", |ctx| LifecycleOps::rewind(ctx, target_phase));
    }
    pub(crate) fn go_back(&mut self) {
        // Pending decisions (git-guard, dreaming) are the legitimate exit path
        // — go_back is a no-op while one is open. Mirrors the legacy
        // GitGuardPending / DreamingPending branches that did nothing.
        if self.pending_decisions.blocks() {
            return;
        }
        let Some(mut target) = self.slim_phase.previous() else {
            self.push_status(
                "nothing to go back to".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        };
        // Implementation(1) has two predecessors depending on whether the
        // operator skipped spec / planning via the skip-to-impl path:
        //   - skip_to_impl_rationale set → rewind all the way to Idea (the
        //     slim phase brainstorm runs at; FSM will re-offer the modal).
        //   - otherwise → Plan, and the legacy `reset_builder_after_rewind`
        //     state mutator must fire to clear the pipeline.
        if matches!(self.slim_phase, SlimPhase::Implementation(1)) {
            if self.state.skip_to_impl_rationale.is_some() {
                target = SlimPhase::Idea;
            } else {
                session_state::reset_builder_after_rewind(&mut self.state);
            }
        }
        // Rewinding away from a phase that owns the skip-to-impl proposal
        // must clear the proposal too. The legacy go_back's SkipToImplPending
        // branch did this inline; preserve it here so the modal doesn't
        // re-fire after a rewind to brainstorm.
        if self.state.current_phase == Phase::SkipToImplPending {
            session_state::clear_skip_to_impl_proposal(&mut self.state);
        }
        self.run_lifecycle_op("back", |ctx| LifecycleOps::rewind(ctx, target));
    }
}

/// Slim phase to rewind to when the operator retries a specific task.
pub(crate) fn slim_phase_for_task_retry(task_id: u32, state: &crate::state::SessionState) -> SlimPhase {
    let max_round = state
        .agent_runs
        .iter()
        .filter(|run| run.task_id == Some(task_id))
        .map(|run| run.round)
        .max();
    let phase_round = match state.current_phase {
        Phase::ImplementationRound(r) | Phase::ReviewRound(r) => Some(r),
        Phase::BuilderRecovery(r)
        | Phase::BuilderRecoveryPlanReview(r)
        | Phase::BuilderRecoverySharding(r) => Some(r),
        _ => None,
    };
    let round = max_round.or(phase_round).unwrap_or(1);
    SlimPhase::Implementation(round)
}

/// Slim phase to rewind to when the operator retries a stage by name.
pub(crate) fn slim_phase_for_stage_retry(stage: &str) -> SlimPhase {
    use crate::logic::rules::retry_phase_for_stage;
    retry_phase_for_stage(stage)
        .map(|p| p.to_slim_phase())
        .unwrap_or(SlimPhase::Plan)
}
