//! Final validation stage: non-mutating idempotent validator launched
//! after the last review round. Runs on [`Phase::Finalization`] and moves
//! the lifecycle to [`Phase::Done`] on success.
//!
//! `supports_restart` is `false` — the validator is meant to be re-derived
//! from inputs, not retried with mutation. The FSM uses the override to
//! disable the `:retry` operator gesture when this stage is the active one.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{Stage, StageCtx, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy)]
pub struct FinalValidationStage;

impl Stage for FinalValidationStage {
    fn id(&self) -> StageId {
        StageId::FinalValidation
    }

    fn label(&self) -> &'static str {
        "Final Validation"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_final_validation` line 113.
        "[FinalValidation]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        StageSpec {
            stage_id: self.id(),
            round: 1,
            task_id: None,
            attempt: next_attempt(ctx, StageId::FinalValidation, None, 1),
            window_name: self.window_name(1, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        if has_succeeded(ctx, StageId::FinalValidation, None, 1) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: next_attempt(ctx, StageId::FinalValidation, None, 1),
            })
        }
    }

    fn supports_restart(&self) -> bool {
        false
    }

    fn phase_when_running(&self) -> Phase {
        Phase::Finalization
    }

    fn next_phase_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Phase {
        // The dreaming-decision modal lives in PendingDecisions; the FSM
        // routes from Done into Dreaming only if the operator opts in.
        Phase::Done
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        // Legacy go_back() does not remove anything for FinalValidation
        // beyond cancelling the run label; preserve that.
        Vec::new()
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("prompts/final-validation.md")]
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
            phase: Phase::Finalization,
            prior_runs: prior,
            pending_task_ids: &[],
            yolo: false,
            cheap: false,
        }
    }

    #[test]
    fn identity_and_window_match_legacy_launch() {
        let s = FinalValidationStage;
        assert_eq!(s.id(), StageId::FinalValidation);
        assert_eq!(s.label(), "Final Validation");
        assert_eq!(s.window_name(1, None), "[FinalValidation]");
        assert_eq!(s.phase_when_running(), Phase::Finalization);
    }

    #[test]
    fn supports_restart_is_false() {
        assert!(!FinalValidationStage.supports_restart());
    }

    #[test]
    fn no_artifacts_or_backups() {
        let s = FinalValidationStage;
        assert!(s.artifact_paths(1).is_empty());
        assert!(s.restore_backups(1).is_empty());
        assert_eq!(
            s.prompt_paths(1),
            vec![PathBuf::from("prompts/final-validation.md")]
        );
    }

    #[test]
    fn next_phase_on_success_is_done() {
        let s = FinalValidationStage;
        let ctx = mk_ctx(&[]);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };
        assert_eq!(s.next_phase_on_success(&ctx, &outcome), Phase::Done);
    }

    #[test]
    fn pending_clears_once_done() {
        let s = FinalValidationStage;
        let prior = [RunHistoryEntry {
            stage_id: StageId::FinalValidation,
            task_id: None,
            round: 1,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        assert!(s.next_pending_work(&mk_ctx(&prior)).is_none());
    }
}
