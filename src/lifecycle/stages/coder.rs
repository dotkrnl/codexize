//! Coder stage: per-task implementation work for an implementation round.
//!
//! Multi-shot per round: each pending task id is one work unit. The stage
//! stays on [`Phase::Implementation(r)`] while more tasks are pending; once
//! every task is Done it moves the lifecycle to [`Phase::Review(r)`].
//!
//! Window labels follow the legacy `launch_coder`'s `[Round {r} Coder]` —
//! the older `[Builder r{r}]` cancel-match prefix in `retry.rs` is stale.
use super::next_attempt;
use crate::lifecycle::fsm::Outcome;
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{Stage, StageCtx, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

/// Implementation round derived from the context. Defaults to `1` if the
/// phase is not an `Implementation(r)`.
fn current_round(ctx: &StageCtx<'_>) -> u32 {
    match ctx.phase {
        Phase::Implementation(r) => r,
        _ => 1,
    }
}

/// Lowest task id in `ctx.pending_task_ids` that does not yet have a Done
/// coder run at the current round, or `None` when the round is complete.
fn next_pending_task(ctx: &StageCtx<'_>, round: u32) -> Option<u32> {
    ctx.pending_task_ids
        .iter()
        .copied()
        .find(|task_id| {
            !ctx.prior_runs.iter().any(|r| {
                r.stage_id == StageId::Coder
                    && r.task_id == Some(*task_id)
                    && r.round == round
                    && r.outcome == Some(Outcome::Done)
            })
        })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CoderStage;

impl Stage for CoderStage {
    fn id(&self) -> StageId {
        StageId::Coder
    }

    fn label(&self) -> &'static str {
        "Coder"
    }

    fn window_name(&self, round: u32, _task: Option<u32>) -> String {
        // Matches `launch_coder` line 98. The per-task suffix is appended
        // by the launcher elsewhere; window_name returns the round-scoped
        // base label.
        format!("[Round {round} Coder]")
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        let task = next_pending_task(ctx, round);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: task,
            attempt: next_attempt(ctx, StageId::Coder, task, round),
            window_name: self.window_name(round, task),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        let task = next_pending_task(ctx, round)?;
        Some(WorkUnit {
            task_id: Some(task),
            round,
            attempt: next_attempt(ctx, StageId::Coder, Some(task), round),
        })
    }

    fn phase_when_running(&self) -> Phase {
        // The exact round is supplied through StageCtx; the trait method
        // returns the canonical Phase::Implementation(1) so the registry's
        // phase→stage lookup keys against the Implementation variant. The
        // FSM consults StageCtx.phase for the real round when scheduling.
        Phase::Implementation(1)
    }

    fn next_phase_on_success(&self, ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Phase {
        let round = current_round(ctx);
        // Stay on Implementation(r) while any task is still pending; once
        // the round is fully Done, the FSM moves us to Review(r).
        if next_pending_task(ctx, round).is_some() {
            Phase::Implementation(round)
        } else {
            Phase::Review(round)
        }
    }

    fn artifact_paths(&self, round: u32) -> Vec<PathBuf> {
        // retry.rs line 404: rewinds remove the entire `rounds/{r:03}` dir.
        vec![PathBuf::from(format!("rounds/{round:03}"))]
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        // Matches `launch_coder` line 55: prompts/coder-r{r}.md.
        vec![PathBuf::from(format!("prompts/coder-r{round}.md"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(
        phase: Phase,
        prior: &'a [RunHistoryEntry],
        pending: &'a [u32],
    ) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            phase,
            prior_runs: prior,
            pending_task_ids: pending,
            yolo: false,
            cheap: false,
        }
    }

    #[test]
    fn identity_and_window_match_legacy_launch() {
        let s = CoderStage;
        assert_eq!(s.id(), StageId::Coder);
        assert_eq!(s.label(), "Coder");
        assert_eq!(s.window_name(1, None), "[Round 1 Coder]");
        assert_eq!(s.window_name(2, Some(5)), "[Round 2 Coder]");
        assert_eq!(s.phase_when_running(), Phase::Implementation(1));
    }

    #[test]
    fn artifact_paths_target_round_directory() {
        let s = CoderStage;
        assert_eq!(s.artifact_paths(1), vec![PathBuf::from("rounds/001")]);
        assert_eq!(s.artifact_paths(12), vec![PathBuf::from("rounds/012")]);
        assert_eq!(
            s.prompt_paths(3),
            vec![PathBuf::from("prompts/coder-r3.md")]
        );
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn next_pending_work_picks_lowest_undone_task() {
        let s = CoderStage;
        let pending = [1u32, 2, 3];
        let prior = [RunHistoryEntry {
            stage_id: StageId::Coder,
            task_id: Some(1),
            round: 1,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        let ctx = mk_ctx(Phase::Implementation(1), &prior, &pending);
        let w = s.next_pending_work(&ctx).expect("work pending");
        assert_eq!(w.task_id, Some(2));
        assert_eq!(w.round, 1);
        assert_eq!(w.attempt, 1);
    }

    #[test]
    fn next_phase_advances_to_review_when_round_done() {
        let s = CoderStage;
        let pending = [1u32];
        let prior = [RunHistoryEntry {
            stage_id: StageId::Coder,
            task_id: Some(1),
            round: 2,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        let ctx = mk_ctx(Phase::Implementation(2), &prior, &pending);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };
        assert_eq!(s.next_phase_on_success(&ctx, &outcome), Phase::Review(2));
    }

    #[test]
    fn next_phase_stays_on_implementation_while_tasks_pending() {
        let s = CoderStage;
        let pending = [1u32, 2];
        let prior: [RunHistoryEntry; 0] = [];
        let ctx = mk_ctx(Phase::Implementation(1), &prior, &pending);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };
        assert_eq!(
            s.next_phase_on_success(&ctx, &outcome),
            Phase::Implementation(1)
        );
    }
}
