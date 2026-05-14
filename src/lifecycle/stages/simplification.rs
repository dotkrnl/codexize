//! Simplification stage: code-producing pass that follows a successful
//! review of a round. Runs on [`Stage::Review(r)`] and stays there on
//! success — the round-loop logic in the FSM decides whether to launch
//! another implementation round or move to finalization.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::Stage;
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

#[derive(Debug, Default, Clone, Copy)]
pub struct SimplificationStage;

impl StageDriver for SimplificationStage {
    fn id(&self) -> StageId {
        StageId::Simplification
    }

    fn label(&self) -> &'static str {
        "Simplification"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_simplifier` line 138.
        "[Simplifier]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: None,
            attempt: next_attempt(ctx, StageId::Simplification, None, round),
            window_name: self.window_name(round, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        if has_succeeded(ctx, StageId::Simplification, None, round) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round,
                attempt: next_attempt(ctx, StageId::Simplification, None, round),
            })
        }
    }

    fn stage_when_running(&self) -> Stage {
        Stage::Review(1)
    }

    fn next_stage_on_success(&self, ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        // Stays on Stage::Review(r); the round loop decides what's next.
        Stage::Review(current_round(ctx))
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        // Persisted go_back() (retry.rs:476) doesn't remove artifacts — it
        // only cancels the run and transitions back to ReviewRound(r).
        Vec::new()
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!("prompts/simplifier-r{round}.md"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(stage: Stage, prior: &'a [RunHistoryEntry]) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            stage,
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
        let s = SimplificationStage;
        assert_eq!(s.id(), StageId::Simplification);
        assert_eq!(s.label(), "Simplification");
        assert_eq!(s.window_name(1, None), "[Simplifier]");
        assert_eq!(s.stage_when_running(), Stage::Review(1));
    }

    #[test]
    fn prompt_paths_vary_with_round() {
        let s = SimplificationStage;
        assert_eq!(
            s.prompt_paths(2),
            vec![PathBuf::from("prompts/simplifier-r2.md")]
        );
        assert!(s.artifact_paths(1).is_empty());
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn build_spec_carries_round_from_stage() {
        let s = SimplificationStage;
        let ctx = mk_ctx(Stage::Review(3), &[]);
        let spec = s.build_spec(&ctx);
        assert_eq!(spec.round, 3);
        assert_eq!(spec.stage_id, StageId::Simplification);
        assert_eq!(spec.window_name, "[Simplifier]");
    }
}
