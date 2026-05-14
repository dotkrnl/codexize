//! Sharding stage: converts `plan.md` into `tasks.toml`. Single-shot,
//! single-round (round = 1). Runs on [`Stage::Plan`] and moves the
//! lifecycle to [`Stage::Implementation(1)`] on success.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::Stage;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy)]
pub struct ShardingStage;

impl StageDriver for ShardingStage {
    fn id(&self) -> StageId {
        StageId::Sharding
    }

    fn label(&self) -> &'static str {
        "Sharding"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_sharding` line 68.
        "[Sharding]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        StageSpec {
            stage_id: self.id(),
            round: 1,
            task_id: None,
            attempt: next_attempt(ctx, StageId::Sharding, None, 1),
            window_name: self.window_name(1, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        if has_succeeded(ctx, StageId::Sharding, None, 1) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: next_attempt(ctx, StageId::Sharding, None, 1),
            })
        }
    }

    fn stage_when_running(&self) -> Stage {
        Stage::Plan
    }

    fn next_stage_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        Stage::Implementation(1)
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("artifacts/tasks.toml")]
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("prompts/sharding.md")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::fsm::Outcome;
    use crate::lifecycle::spec::ActiveRun;
    use crate::lifecycle::stage::{RunHistoryEntry, SuccessOutcome};
    use chrono::Utc;
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
        let s = ShardingStage;
        assert_eq!(s.id(), StageId::Sharding);
        assert_eq!(s.label(), "Sharding");
        assert_eq!(s.window_name(1, None), "[Sharding]");
        assert_eq!(s.stage_when_running(), Stage::Plan);
    }

    #[test]
    fn paths_match_go_back_cleanup() {
        let s = ShardingStage;
        assert_eq!(
            s.artifact_paths(1),
            vec![PathBuf::from("artifacts/tasks.toml")]
        );
        assert_eq!(
            s.prompt_paths(1),
            vec![PathBuf::from("prompts/sharding.md")]
        );
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn success_routes_to_implementation_one() {
        let s = ShardingStage;
        let ctx = mk_ctx(&[]);
        let outcome = SuccessOutcome {
            run: ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: Utc::now(),
            },
        };
        assert_eq!(
            s.next_stage_on_success(&ctx, &outcome),
            Stage::Implementation(1)
        );
    }

    #[test]
    fn next_pending_work_clears_when_done() {
        let s = ShardingStage;
        assert!(s.next_pending_work(&mk_ctx(&[])).is_some());
        let prior = [RunHistoryEntry {
            stage_id: StageId::Sharding,
            task_id: None,
            round: 1,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        assert!(s.next_pending_work(&mk_ctx(&prior)).is_none());
    }
}
