//! Dreaming stage: optional post-final-validation pass the operator can
//! opt into via the dreaming-decision modal. Runs on [`Stage::Finalization`]
//! and moves the lifecycle to [`Stage::Done`] on success.
//!
//! `supports_restart` is `false` — the persisted `go_back()` only cancels the
//! run label and leaves no path to relaunch through `:retry`.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::Stage;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{StageCtx, StageDriver, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy)]
pub struct DreamingStage;

impl StageDriver for DreamingStage {
    fn id(&self) -> StageId {
        StageId::Dreaming
    }

    fn label(&self) -> &'static str {
        "Dreaming"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_dreaming` line 98.
        "[Dreaming]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        StageSpec {
            stage_id: self.id(),
            round: 1,
            task_id: None,
            attempt: next_attempt(ctx, StageId::Dreaming, None, 1),
            window_name: self.window_name(1, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        if has_succeeded(ctx, StageId::Dreaming, None, 1) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: next_attempt(ctx, StageId::Dreaming, None, 1),
            })
        }
    }

    fn supports_restart(&self) -> bool {
        false
    }

    fn stage_when_running(&self) -> Stage {
        Stage::Finalization
    }

    fn next_stage_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Stage {
        Stage::Done
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        Vec::new()
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("prompts/dreaming.md")]
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
            stage: Stage::Finalization,
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
        let s = DreamingStage;
        assert_eq!(s.id(), StageId::Dreaming);
        assert_eq!(s.label(), "Dreaming");
        assert_eq!(s.window_name(1, None), "[Dreaming]");
        assert_eq!(s.stage_when_running(), Stage::Finalization);
    }

    #[test]
    fn supports_restart_is_false() {
        assert!(!DreamingStage.supports_restart());
    }

    #[test]
    fn next_stage_is_done() {
        let s = DreamingStage;
        let ctx = mk_ctx(&[]);
        let outcome = SuccessOutcome {
            run: crate::lifecycle::spec::ActiveRun {
                run_id: 1,
                spec: s.build_spec(&ctx),
                started_at: chrono::Utc::now(),
            },
        };
        assert_eq!(s.next_stage_on_success(&ctx, &outcome), Stage::Done);
    }

    #[test]
    fn no_artifacts_or_backups() {
        let s = DreamingStage;
        assert!(s.artifact_paths(1).is_empty());
        assert!(s.restore_backups(1).is_empty());
        assert_eq!(
            s.prompt_paths(1),
            vec![PathBuf::from("prompts/dreaming.md")]
        );
    }
}
