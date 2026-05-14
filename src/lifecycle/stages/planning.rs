//! Planning stage: produces `artifacts/plan.md` from the approved spec.
//!
//! Single-shot, single-round. Runs while the session sits on
//! [`Phase::Plan`]; stays in [`Phase::Plan`] on success so plan-review
//! (also a Phase::Plan stage) can pick up next.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{Stage, StageCtx, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy)]
pub struct PlanningStage;

impl Stage for PlanningStage {
    fn id(&self) -> StageId {
        StageId::Planning
    }

    fn label(&self) -> &'static str {
        "Planning"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_planning` line 83.
        "[Planning]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        StageSpec {
            stage_id: self.id(),
            round: 1,
            task_id: None,
            attempt: next_attempt(ctx, StageId::Planning, None, 1),
            window_name: self.window_name(1, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        if has_succeeded(ctx, StageId::Planning, None, 1) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: next_attempt(ctx, StageId::Planning, None, 1),
            })
        }
    }

    fn phase_when_running(&self) -> Phase {
        Phase::Plan
    }

    fn next_phase_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Phase {
        // Stays on Phase::Plan; plan-review (also Phase::Plan) picks up the
        // baton. The legacy code's WaitingToImplement YOLO shortcut and
        // PlanApproval modal both live in PendingDecisions now.
        Phase::Plan
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("artifacts/plan.md")]
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("prompts/planning.md")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::fsm::Outcome;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(prior: &'a [RunHistoryEntry]) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            phase: Phase::Plan,
            prior_runs: prior,
            pending_task_ids: &[],
            yolo: false,
            cheap: false,
            recovery_active: false,
            simplification_requested: false,
            dreaming_accepted: false,
        }
    }

    #[test]
    fn identity_and_window_match_legacy_launch() {
        let s = PlanningStage;
        assert_eq!(s.id(), StageId::Planning);
        assert_eq!(s.label(), "Planning");
        assert_eq!(s.window_name(1, None), "[Planning]");
        assert_eq!(s.phase_when_running(), Phase::Plan);
    }

    #[test]
    fn paths_match_go_back_cleanup() {
        let s = PlanningStage;
        assert_eq!(s.artifact_paths(1), vec![PathBuf::from("artifacts/plan.md")]);
        assert_eq!(s.prompt_paths(1), vec![PathBuf::from("prompts/planning.md")]);
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn next_pending_work_clears_when_done() {
        let s = PlanningStage;
        assert!(s.next_pending_work(&mk_ctx(&[])).is_some());
        let prior = [RunHistoryEntry {
            stage_id: StageId::Planning,
            task_id: None,
            round: 1,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        assert!(s.next_pending_work(&mk_ctx(&prior)).is_none());
    }

    #[test]
    fn build_spec_uses_planning_stage_id() {
        let s = PlanningStage;
        let spec = s.build_spec(&mk_ctx(&[]));
        assert_eq!(spec.stage_id, StageId::Planning);
        assert_eq!(spec.window_name, "[Planning]");
    }
}
