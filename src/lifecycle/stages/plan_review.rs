//! Plan review stage: per-round review of `artifacts/plan.md`. Produces
//! `artifacts/plan-review-{round}.md`.
//!
//! Single-shot per round. Runs on [`Stage::Plan`]. Rewind restores
//! `plan.pre-review-1.md`/`spec.pre-review-1.md` only for round 1; later
//! rounds overwrite `plan.md` in place without restore backups.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::Stage;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

fn current_round(ctx: &StageCtx<'_>) -> u32 {
    let max_done = ctx
        .prior_runs
        .iter()
        .filter(|r| {
            r.stage_id == StageId::PlanReview
                && r.outcome == Some(crate::lifecycle::fsm::Outcome::Done)
        })
        .map(|r| r.round)
        .max();
    match max_done {
        Some(n) => n.saturating_add(1),
        None => 1,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PlanReviewStage;

impl StageDriver for PlanReviewStage {
    fn id(&self) -> StageId {
        StageId::PlanReview
    }

    fn label(&self) -> &'static str {
        "Plan Review"
    }

    fn window_name(&self, round: u32, _task: Option<u32>) -> String {
        // Matches `launch_plan_review` line 104.
        format!("[Plan Review {round}]")
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: None,
            attempt: next_attempt(ctx, StageId::PlanReview, None, round),
            window_name: self.window_name(round, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        if has_succeeded(ctx, StageId::PlanReview, None, round) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round,
                attempt: next_attempt(ctx, StageId::PlanReview, None, round),
            })
        }
    }

    fn stage_when_running(&self) -> Stage {
        Stage::Plan
    }

    fn next_stage_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        // Stays on Stage::Plan; sharding (also Stage::Plan) takes over once
        // the operator approves via PendingDecisions and the
        // repo-state-update baseline check clears.
        Stage::Plan
    }

    fn artifact_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!("artifacts/plan-review-{round}.md"))]
    }

    fn restore_backups(&self, round: u32) -> Vec<(PathBuf, PathBuf)> {
        if round == 1 {
            vec![
                (
                    PathBuf::from("artifacts/plan.pre-review-1.md"),
                    PathBuf::from("artifacts/plan.md"),
                ),
                (
                    PathBuf::from("artifacts/spec.pre-review-1.md"),
                    PathBuf::from("artifacts/spec.md"),
                ),
            ]
        } else {
            Vec::new()
        }
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!("prompts/plan-review-{round}.md"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(prior: &'a [RunHistoryEntry]) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            stage: Stage::Plan,
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
    fn identity_and_window_match_persisted_launch() {
        let s = PlanReviewStage;
        assert_eq!(s.id(), StageId::PlanReview);
        assert_eq!(s.label(), "Plan Review");
        assert_eq!(s.window_name(1, None), "[Plan Review 1]");
        assert_eq!(s.window_name(2, None), "[Plan Review 2]");
        assert_eq!(s.stage_when_running(), Stage::Plan);
    }

    #[test]
    fn paths_vary_with_round() {
        let s = PlanReviewStage;
        assert_eq!(
            s.artifact_paths(2),
            vec![PathBuf::from("artifacts/plan-review-2.md")]
        );
        assert_eq!(
            s.prompt_paths(2),
            vec![PathBuf::from("prompts/plan-review-2.md")]
        );
    }

    #[test]
    fn restore_backups_only_for_round_one() {
        let s = PlanReviewStage;
        assert_eq!(
            s.restore_backups(1),
            vec![
                (
                    PathBuf::from("artifacts/plan.pre-review-1.md"),
                    PathBuf::from("artifacts/plan.md"),
                ),
                (
                    PathBuf::from("artifacts/spec.pre-review-1.md"),
                    PathBuf::from("artifacts/spec.md"),
                ),
            ]
        );
        assert!(s.restore_backups(2).is_empty());
    }

    #[test]
    fn build_spec_uses_plan_review_stage_id() {
        let s = PlanReviewStage;
        let spec = s.build_spec(&mk_ctx(&[]));
        assert_eq!(spec.stage_id, StageId::PlanReview);
        assert_eq!(spec.window_name, "[Plan Review 1]");
    }
}
