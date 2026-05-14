//! Coder stage: per-task implementation work for an implementation round.
//!
//! Multi-shot per round: each pending task id is one work unit. The stage
//! stays on [`Stage::Implementation(r)`] while more tasks are pending; once
//! every task is Done it moves the lifecycle to [`Stage::Review(r)`].
//!
//! Window labels follow the persisted `launch_coder`'s `[Round {r} Coder]` —
//! the older `[Builder r{r}]` cancel-match prefix in `retry.rs` is stale.
use super::next_attempt;
use crate::lifecycle::Stage;
use crate::lifecycle::fsm::Outcome;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

/// Implementation round derived from the context. Defaults to `1` if the
/// stage is not an `Implementation(r)`.
fn current_round(ctx: &StageCtx<'_>) -> u32 {
    match ctx.stage {
        Stage::Implementation(r) => r,
        _ => 1,
    }
}

/// Lowest task id in `ctx.pending_task_ids` that does not yet have a Done
/// coder run at the current round, or `None` when the round is complete.
fn next_pending_task(ctx: &StageCtx<'_>, round: u32) -> Option<u32> {
    ctx.pending_task_ids.iter().copied().find(|task_id| {
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

impl StageDriver for CoderStage {
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

    fn stage_when_running(&self) -> Stage {
        // The exact round is supplied through StageCtx; the trait method
        // returns the canonical Stage::Implementation(1) so the registry's
        // stage→stage lookup keys against the Implementation variant. The
        // FSM consults StageCtx.stage for the real round when scheduling.
        Stage::Implementation(1)
    }

    fn next_stage_on_success(&self, ctx: &StageCtx<'_>, outcome: &SuccessOutcome) -> Stage {
        let round = current_round(ctx);
        let completed_task =
            if outcome.run.spec.stage_id == StageId::Coder && outcome.run.spec.round == round {
                outcome.run.spec.task_id
            } else {
                None
            };
        // Stay on Implementation(r) while any task is still pending; once
        // the round is fully Done, the FSM moves us to Review(r).
        let has_pending = ctx.pending_task_ids.iter().copied().any(|task_id| {
            Some(task_id) != completed_task
                && !ctx.prior_runs.iter().any(|r| {
                    r.stage_id == StageId::Coder
                        && r.task_id == Some(task_id)
                        && r.round == round
                        && r.outcome == Some(Outcome::Done)
                })
        });
        if has_pending {
            Stage::Implementation(round)
        } else {
            Stage::Review(round)
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
        let s = CoderStage;
        assert_eq!(s.id(), StageId::Coder);
        assert_eq!(s.label(), "Coder");
        assert_eq!(s.window_name(1, None), "[Round 1 Coder]");
        assert_eq!(s.window_name(2, Some(5)), "[Round 2 Coder]");
        assert_eq!(s.stage_when_running(), Stage::Implementation(1));
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
        let ctx = mk_ctx(Stage::Implementation(1), &prior, &pending);
        let w = s.next_pending_work(&ctx).expect("work pending");
        assert_eq!(w.task_id, Some(2));
        assert_eq!(w.round, 1);
        assert_eq!(w.attempt, 1);
    }

    #[test]
    fn next_stage_advances_to_review_when_round_done() {
        let s = CoderStage;
        let pending = [1u32];
        let prior = [RunHistoryEntry {
            stage_id: StageId::Coder,
            task_id: Some(1),
            round: 2,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        let ctx = mk_ctx(Stage::Implementation(2), &prior, &pending);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };
        assert_eq!(s.next_stage_on_success(&ctx, &outcome), Stage::Review(2));
    }

    #[test]
    fn next_stage_counts_successful_run_before_prior_history_updates() {
        let s = CoderStage;
        let pending = [1u32];
        let prior: [RunHistoryEntry; 0] = [];
        let ctx = mk_ctx(Stage::Implementation(2), &prior, &pending);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };

        assert_eq!(s.next_stage_on_success(&ctx, &outcome), Stage::Review(2));
    }

    #[test]
    fn next_stage_stays_on_implementation_while_tasks_pending() {
        let s = CoderStage;
        let pending = [1u32, 2];
        let prior: [RunHistoryEntry; 0] = [];
        let ctx = mk_ctx(Stage::Implementation(1), &prior, &pending);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };
        assert_eq!(
            s.next_stage_on_success(&ctx, &outcome),
            Stage::Implementation(1)
        );
    }
}
