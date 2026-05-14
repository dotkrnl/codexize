use super::{
    RetryTarget, Severity, parse_task_label_id, retry_stage_for_label, retry_target_for_run,
};
use crate::app::App;
use crate::app::tree::node_at_path;
use crate::lifecycle::{
    LifecycleOps, Phase as SlimPhase, slim_phase_for_stage_retry, slim_phase_for_task_retry,
};
use crate::logic::rules::retry_phase_for_stage;
use crate::scheduler::{is_implementation_lane_phase, manual_retry_allowed};
use crate::state::{self as session_state, NodeKind, Phase};
use std::time::Duration;
impl App {
    pub(crate) fn retry_allowed_by_project_lane(&mut self, target_phase: Phase) -> bool {
        if !is_implementation_lane_phase(target_phase) {
            return true;
        }
        let sessions_root = crate::picker::sessions_root_for(&self.config);
        let scan = match crate::data::picker_io::scan_sessions_for_scheduler(&sessions_root) {
            Ok(scan) => scan,
            Err(err) => {
                let message = format!("retry blocked: cannot scan sessions for lane gate: {err:#}");
                self.surface_boundary_error(message, false);
                return false;
            }
        };
        if manual_retry_allowed(target_phase, &self.state.session_id, &scan) {
            return true;
        }
        if !scan
            .iter()
            .any(|entry| entry.session_id() == self.state.session_id)
        {
            // Isolated App tests and path-based sessions can run outside the
            // project scan; only shell-owned focused sessions can be lane-gated.
            return true;
        }
        // Manual implementation retries share the scheduler's project-wide
        // lane gate; otherwise a focused session could start sharding or
        // later work while a background session is already mutating the repo.
        let message = "retry blocked: implementation lane is occupied by another session";
        let _ = self.state.log_event(message);
        self.push_status(message.to_string(), Severity::Warn, Duration::from_secs(5));
        false
    }

    pub(crate) fn retry_gate_phase_for_stage(stage: &str) -> Option<Phase> {
        match stage {
            "sharding" => Some(Phase::ShardingRunning),
            "repo-state-update" => Some(Phase::RepoStateUpdateRunning),
            _ => retry_phase_for_stage(stage),
        }
    }

    pub(crate) fn retry_gate_phase_for_stage_id(stage_id: crate::app::StageId) -> Phase {
        match stage_id {
            crate::app::StageId::Brainstorm => Phase::BrainstormRunning,
            crate::app::StageId::SpecReview => Phase::SpecReviewRunning,
            crate::app::StageId::Planning => Phase::PlanningRunning,
            crate::app::StageId::PlanReview => Phase::PlanReviewRunning,
            crate::app::StageId::Sharding => Phase::ShardingRunning,
            crate::app::StageId::Implementation => Phase::ImplementationRound(1),
            crate::app::StageId::Review => Phase::ReviewRound(1),
            crate::app::StageId::FinalValidation => Phase::FinalValidation(1),
            crate::app::StageId::Dreaming => Phase::Dreaming(1),
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
        if self.pending_decisions.blocks(self.slim_phase) {
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
