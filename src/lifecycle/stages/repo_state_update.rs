//! Repo-state update stage: re-baselines a `WaitingToImplement` session's
//! spec/plan against the current repository state before sharding launches.
//! Runs on [`Phase::Plan`] (right before sharding) and stays on
//! [`Phase::Plan`] on success so the FSM can dispatch sharding.
//!
//! Per the spec §AC-6, a `not_implementable` verdict is a routed outcome
//! the FSM consumes via [`super::super::PendingDecisions`] — not a separate
//! phase. The Stage trait's success path returns Phase::Plan and the FSM
//! reads the verdict to decide whether to dispatch sharding or block.
use super::{has_succeeded, next_attempt};
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{Stage, StageCtx, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy)]
pub struct RepoStateUpdateStage;

impl Stage for RepoStateUpdateStage {
    fn id(&self) -> StageId {
        StageId::RepoStateUpdate
    }

    fn label(&self) -> &'static str {
        "Repo State Update"
    }

    fn window_name(&self, _round: u32, _task: Option<u32>) -> String {
        // Matches `launch_repo_state_update` line 145.
        "[RepoStateUpdate]".to_string()
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        StageSpec {
            stage_id: self.id(),
            round: 1,
            task_id: None,
            attempt: next_attempt(ctx, StageId::RepoStateUpdate, None, 1),
            window_name: self.window_name(1, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        if has_succeeded(ctx, StageId::RepoStateUpdate, None, 1) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: next_attempt(ctx, StageId::RepoStateUpdate, None, 1),
            })
        }
    }

    fn phase_when_running(&self) -> Phase {
        Phase::Plan
    }

    fn next_phase_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Phase {
        // A `not_implementable` verdict blocks via PendingDecisions.plan_approval
        // while the slim phase stays on Plan.
        Phase::Plan
    }

    fn artifact_paths(&self, _round: u32) -> Vec<PathBuf> {
        // The launcher pre-removes `artifacts/repo_state_update.toml`
        // (line 61), but the legacy `go_back()` does not have a branch
        // for RepoStateUpdateRunning — it falls through the noop arm at
        // retry.rs:488-493. Match that: no rewind-time cleanup.
        Vec::new()
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, _round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from("prompts/repo-state-update.md")]
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
            phase: Phase::Plan,
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
    fn identity_and_window_match_legacy_launch() {
        let s = RepoStateUpdateStage;
        assert_eq!(s.id(), StageId::RepoStateUpdate);
        assert_eq!(s.label(), "Repo State Update");
        assert_eq!(s.window_name(1, None), "[RepoStateUpdate]");
        assert_eq!(s.phase_when_running(), Phase::Plan);
    }

    #[test]
    fn no_artifacts_or_backups() {
        let s = RepoStateUpdateStage;
        assert!(s.artifact_paths(1).is_empty());
        assert!(s.restore_backups(1).is_empty());
        assert_eq!(
            s.prompt_paths(1),
            vec![PathBuf::from("prompts/repo-state-update.md")]
        );
    }

    #[test]
    fn build_spec_emits_repo_state_update_stage_id() {
        let s = RepoStateUpdateStage;
        let spec = s.build_spec(&mk_ctx(&[]));
        assert_eq!(spec.stage_id, StageId::RepoStateUpdate);
        assert_eq!(spec.window_name, "[RepoStateUpdate]");
    }
}
