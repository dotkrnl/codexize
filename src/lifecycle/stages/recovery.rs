//! Recovery stage: builder-only intervention launched mid-implementation
//! when the reviewer escalates. Runs inside [`Stage::Implementation(r)`]
//! and stays there on success — recovery-plan-review / recovery-sharding
//! pick up next on the same stage.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::Stage;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

fn current_round(ctx: &StageCtx<'_>) -> u32 {
    match ctx.stage {
        Stage::Implementation(r) => r,
        _ => 1,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RecoveryStage;

impl StageDriver for RecoveryStage {
    fn id(&self) -> StageId {
        StageId::Recovery
    }

    fn label(&self) -> &'static str {
        "Recovery"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_recovery` line 110.
        "[Recovery]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: None,
            attempt: next_attempt(ctx, StageId::Recovery, None, round),
            window_name: self.window_name(round, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        if has_succeeded(ctx, StageId::Recovery, None, round) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round,
                attempt: next_attempt(ctx, StageId::Recovery, None, round),
            })
        }
    }

    fn stage_when_running(&self) -> Stage {
        // Round comes from StageCtx; registry key uses Implementation(1).
        Stage::Implementation(1)
    }

    fn next_stage_on_success(&self, ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        Stage::Implementation(current_round(ctx))
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        // Recovery produces `rounds/{r:03}/recovery.toml` but the persisted
        // rewind cleanup removes only prompts for same-stage recovery runs.
        // Later rewinds that leave implementation delete the owning
        // `rounds/{r:03}` directory through the coder stage.
        Vec::new()
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!("prompts/recovery-r{round}.md"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::fsm::Outcome;
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
        let s = RecoveryStage;
        assert_eq!(s.id(), StageId::Recovery);
        assert_eq!(s.label(), "Recovery");
        assert_eq!(s.window_name(1, None), "[Recovery]");
        assert_eq!(s.window_name(4, None), "[Recovery]");
        assert_eq!(s.stage_when_running(), Stage::Implementation(1));
    }

    #[test]
    fn prompts_carry_round_no_artifacts_or_backups() {
        let s = RecoveryStage;
        assert!(s.artifact_paths(1).is_empty());
        assert!(s.restore_backups(1).is_empty());
        assert_eq!(
            s.prompt_paths(2),
            vec![PathBuf::from("prompts/recovery-r2.md")]
        );
    }

    #[test]
    fn build_spec_carries_round_from_context() {
        let s = RecoveryStage;
        let ctx = mk_ctx(Stage::Implementation(3), &[]);
        let spec = s.build_spec(&ctx);
        assert_eq!(spec.stage_id, StageId::Recovery);
        assert_eq!(spec.round, 3);
        assert_eq!(spec.window_name, "[Recovery]");
    }

    #[test]
    fn next_pending_work_clears_when_done_for_round() {
        let s = RecoveryStage;
        let ctx = mk_ctx(Stage::Implementation(2), &[]);
        assert!(s.next_pending_work(&ctx).is_some());
        let prior = [RunHistoryEntry {
            stage_id: StageId::Recovery,
            task_id: None,
            round: 2,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        assert!(
            s.next_pending_work(&mk_ctx(Stage::Implementation(2), &prior))
                .is_none()
        );
    }
}
