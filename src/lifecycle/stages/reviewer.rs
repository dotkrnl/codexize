//! Reviewer stage: per-task review of an implementation round.
//!
//! Multi-shot per round (same shape as the coder). Stays on
//! [`Stage::Review(r)`] while reviewer tasks remain; once the round is
//! fully Done the FSM decides whether to roll into another implementation
//! round or finalize — that decision lives in the round-loop logic, not
//! in this stage.
use super::next_attempt;
use crate::lifecycle::Stage;
use crate::lifecycle::fsm::Outcome;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

fn current_round(ctx: &StageCtx<'_>) -> u32 {
    match ctx.stage {
        Stage::Review(r) => r,
        _ => 1,
    }
}

fn next_pending_task(ctx: &StageCtx<'_>, round: u32) -> Option<u32> {
    ctx.pending_task_ids.iter().copied().find(|task_id| {
        !ctx.prior_runs.iter().any(|r| {
            r.stage_id == StageId::Reviewer
                && r.task_id == Some(*task_id)
                && r.round == round
                && r.outcome == Some(Outcome::Done)
        })
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ReviewerStage;

impl StageDriver for ReviewerStage {
    fn id(&self) -> StageId {
        StageId::Reviewer
    }

    fn label(&self) -> &'static str {
        "Reviewer"
    }

    fn window_name(&self, round: u32, _task: Option<u32>) -> String {
        // Matches `launch_reviewer` line 142.
        format!("[Round {round} Reviewer]")
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        let task = next_pending_task(ctx, round);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: task,
            attempt: next_attempt(ctx, StageId::Reviewer, task, round),
            window_name: self.window_name(round, task),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        let task = next_pending_task(ctx, round)?;
        Some(WorkUnit {
            task_id: Some(task),
            round,
            attempt: next_attempt(ctx, StageId::Reviewer, Some(task), round),
        })
    }

    fn stage_when_running(&self) -> Stage {
        // Round comes from StageCtx; the registry key uses the canonical
        // Review(1).
        Stage::Review(1)
    }

    fn next_stage_on_success(&self, ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        // Stay on Review(r). The scheduler decides whether to launch
        // Implementation(r+1), FinalValidation, or stay based on the
        // builder queue state and PendingDecisions, not this return value.
        Stage::Review(current_round(ctx))
    }

    fn artifact_paths(&self, round: u32) -> Vec<PathBuf> {
        // Reviewer rewinds also drop the whole round directory in the
        // persisted `go_back()` (retry.rs:422). The dir is shared with the
        // coder stage; whichever stage rewinds first wins the cleanup.
        vec![PathBuf::from(format!("rounds/{round:03}"))]
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        // Matches `launch_reviewer` line 94: prompts/reviewer-r{r}.md.
        vec![PathBuf::from(format!("prompts/reviewer-r{round}.md"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(stage: Stage, prior: &'a [RunHistoryEntry], pending: &'a [u32]) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            stage,
            prior_runs: prior,
            pending_task_ids: pending,
            yolo: false,
            cheap: false,
            recovery_active: false,
            simplification_requested: false,
            dreaming_accepted: false,
        }
    }

    #[test]
    fn identity_and_window_match_persisted_launch() {
        let s = ReviewerStage;
        assert_eq!(s.id(), StageId::Reviewer);
        assert_eq!(s.label(), "Reviewer");
        assert_eq!(s.window_name(1, None), "[Round 1 Reviewer]");
        assert_eq!(s.window_name(3, Some(2)), "[Round 3 Reviewer]");
        assert_eq!(s.stage_when_running(), Stage::Review(1));
    }

    #[test]
    fn artifact_and_prompt_paths_vary_with_round() {
        let s = ReviewerStage;
        assert_eq!(s.artifact_paths(2), vec![PathBuf::from("rounds/002")]);
        assert_eq!(
            s.prompt_paths(4),
            vec![PathBuf::from("prompts/reviewer-r4.md")]
        );
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn next_pending_task_filters_done_runs_for_round() {
        let s = ReviewerStage;
        let pending = [1u32, 2, 3];
        let prior = [
            RunHistoryEntry {
                stage_id: StageId::Reviewer,
                task_id: Some(1),
                round: 1,
                attempt: 1,
                outcome: Some(Outcome::Done),
            },
            // Round 2 done for task 1 should NOT mask round-1 pending state.
            RunHistoryEntry {
                stage_id: StageId::Reviewer,
                task_id: Some(2),
                round: 2,
                attempt: 1,
                outcome: Some(Outcome::Done),
            },
        ];
        let ctx = mk_ctx(Stage::Review(1), &prior, &pending);
        let w = s.next_pending_work(&ctx).expect("work pending");
        assert_eq!(w.task_id, Some(2));
        assert_eq!(w.round, 1);
    }

    #[test]
    fn build_spec_emits_reviewer_stage_id() {
        let s = ReviewerStage;
        let pending = [1u32];
        let ctx = mk_ctx(Stage::Review(2), &[], &pending);
        let spec = s.build_spec(&ctx);
        assert_eq!(spec.stage_id, StageId::Reviewer);
        assert_eq!(spec.round, 2);
        assert_eq!(spec.task_id, Some(1));
        assert_eq!(spec.window_name, "[Round 2 Reviewer]");
    }
}
