//! Brainstorm stage: produces `artifacts/spec.md` from the operator's idea.
//!
//! Single-shot, single-round. Runs while the session sits on
//! [`Stage::Idea`] and lands on [`Stage::Spec`] on success.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::Stage;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

/// Empty marker struct; behavior lives entirely on the trait impl.
#[derive(Debug, Default, Clone, Copy)]
pub struct BrainstormStage;

impl StageDriver for BrainstormStage {
    fn id(&self) -> StageId {
        StageId::Brainstorm
    }

    fn label(&self) -> &'static str {
        "Brainstorm"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_brainstorm` line 100.
        "[Brainstorm]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        StageSpec {
            stage_id: self.id(),
            round: 1,
            task_id: None,
            attempt: next_attempt(ctx, StageId::Brainstorm, None, 1),
            window_name: self.window_name(1, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        if has_succeeded(ctx, StageId::Brainstorm, None, 1) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: next_attempt(ctx, StageId::Brainstorm, None, 1),
            })
        }
    }

    fn stage_when_running(&self) -> Stage {
        Stage::Idea
    }

    fn next_stage_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        // SkipToImpl / SpecApproval are modeled via PendingDecisions, not
        // by overloading the stage enum. The brainstorm stage simply moves
        // the lifecycle to Spec; the FSM consults pending decisions to
        // route into spec review or jump to implementation.
        Stage::Spec
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("artifacts/spec.md")]
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("prompts/brainstorm.md")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::fsm::Outcome;
    use crate::lifecycle::stage::RunHistoryEntry;
    use std::path::Path;

    fn mk_ctx<'a>(prior: &'a [RunHistoryEntry], pending: &'a [u32]) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            stage: Stage::Idea,
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
    fn identity_and_label_match_persisted_launch() {
        let s = BrainstormStage;
        assert_eq!(s.id(), StageId::Brainstorm);
        assert_eq!(s.label(), "Brainstorm");
        assert_eq!(s.window_name(1, None), "[Brainstorm]");
        assert_eq!(s.stage_when_running(), Stage::Idea);
    }

    #[test]
    fn artifact_and_prompt_paths_match_go_back_cleanup() {
        let s = BrainstormStage;
        assert_eq!(
            s.artifact_paths(1),
            vec![PathBuf::from("artifacts/spec.md")]
        );
        assert_eq!(
            s.prompt_paths(1),
            vec![PathBuf::from("prompts/brainstorm.md")]
        );
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn next_pending_work_is_some_until_done() {
        let s = BrainstormStage;
        let ctx = mk_ctx(&[], &[]);
        assert_eq!(
            s.next_pending_work(&ctx),
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: 1
            })
        );
        let prior = [RunHistoryEntry {
            stage_id: StageId::Brainstorm,
            task_id: None,
            round: 1,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        assert!(s.next_pending_work(&mk_ctx(&prior, &[])).is_none());
    }

    #[test]
    fn build_spec_uses_brainstorm_stage_id_and_window() {
        let s = BrainstormStage;
        let spec = s.build_spec(&mk_ctx(&[], &[]));
        assert_eq!(spec.stage_id, StageId::Brainstorm);
        assert_eq!(spec.round, 1);
        assert_eq!(spec.task_id, None);
        assert_eq!(spec.window_name, "[Brainstorm]");
    }
}
