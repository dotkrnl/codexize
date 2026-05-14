//! Recovery sharding stage: re-shards the plan inside an implementation
//! round after recovery, producing a fresh `tasks.toml`. Runs on
//! [`Phase::Implementation(r)`] and stays there on success so the coder
//! picks the next pending task.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{Stage, StageCtx, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

fn current_round(ctx: &StageCtx<'_>) -> u32 {
    match ctx.phase {
        Phase::Implementation(r) => r,
        _ => 1,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RecoveryShardingStage;

impl Stage for RecoveryShardingStage {
    fn id(&self) -> StageId {
        StageId::RecoverySharding
    }

    fn label(&self) -> &'static str {
        "Recovery Sharding"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_recovery_sharding` line 85.
        "[Recovery Sharding]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: None,
            attempt: next_attempt(ctx, StageId::RecoverySharding, None, round),
            window_name: self.window_name(round, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        if has_succeeded(ctx, StageId::RecoverySharding, None, round) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round,
                attempt: next_attempt(ctx, StageId::RecoverySharding, None, round),
            })
        }
    }

    fn phase_when_running(&self) -> Phase {
        Phase::Implementation(1)
    }

    fn next_phase_on_success(&self, ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Phase {
        Phase::Implementation(current_round(ctx))
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        // Recovery sharding mutates `artifacts/tasks.toml`, but the legacy
        // `go_back()` does not clean it (it only removes the prompt). Keep
        // that asymmetry — Step 8 may revisit.
        Vec::new()
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!(
            "prompts/recovery-sharding-r{round}.md"
        ))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(phase: Phase, prior: &'a [RunHistoryEntry]) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            phase,
            prior_runs: prior,
            pending_task_ids: &[],
            yolo: false,
            cheap: false,
        }
    }

    #[test]
    fn identity_and_window_match_legacy_launch() {
        let s = RecoveryShardingStage;
        assert_eq!(s.id(), StageId::RecoverySharding);
        assert_eq!(s.label(), "Recovery Sharding");
        assert_eq!(s.window_name(1, None), "[Recovery Sharding]");
        assert_eq!(s.phase_when_running(), Phase::Implementation(1));
    }

    #[test]
    fn prompt_paths_vary_with_round() {
        let s = RecoveryShardingStage;
        assert_eq!(
            s.prompt_paths(3),
            vec![PathBuf::from("prompts/recovery-sharding-r3.md")]
        );
        assert!(s.artifact_paths(1).is_empty());
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn build_spec_carries_round_from_phase() {
        let s = RecoveryShardingStage;
        let ctx = mk_ctx(Phase::Implementation(5), &[]);
        let spec = s.build_spec(&ctx);
        assert_eq!(spec.round, 5);
        assert_eq!(spec.stage_id, StageId::RecoverySharding);
        assert_eq!(spec.window_name, "[Recovery Sharding]");
    }
}
