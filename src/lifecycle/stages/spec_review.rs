//! Spec review stage: per-round review of `artifacts/spec.md` that produces
//! `artifacts/spec-review-{round}.md`.
//!
//! Single-shot per round. Runs while the session sits on [`Phase::Spec`].
//! On success the FSM consults pending decisions (spec approval modal) and
//! either re-launches another round or moves to [`Phase::Plan`].
use super::{has_succeeded, next_attempt};
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::StageSpec;
use crate::lifecycle::stage::{Stage, StageCtx, SuccessOutcome, WorkUnit};
use crate::lifecycle::stage_id::StageId;
use std::path::PathBuf;

/// Round counter for spec review reads off `ctx.prior_runs`. The active
/// round is `latest_round + 1` once the previous round is Done; otherwise
/// it stays on the latest round. Single-round impls and tests can pass
/// `round = 1` directly.
fn current_round(ctx: &StageCtx<'_>) -> u32 {
    let max_done = ctx
        .prior_runs
        .iter()
        .filter(|r| {
            r.stage_id == StageId::SpecReview && r.outcome == Some(crate::lifecycle::fsm::Outcome::Done)
        })
        .map(|r| r.round)
        .max();
    match max_done {
        Some(n) => n.saturating_add(1),
        None => 1,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SpecReviewStage;

impl Stage for SpecReviewStage {
    fn id(&self) -> StageId {
        StageId::SpecReview
    }

    fn label(&self) -> &'static str {
        "Spec Review"
    }

    fn window_name(&self, round: u32, _task: Option<u32>) -> String {
        // Matches `launch_spec_review` line 101: `format!("[Spec Review {round}]")`.
        format!("[Spec Review {round}]")
    }

    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec {
        let round = current_round(ctx);
        StageSpec {
            stage_id: self.id(),
            round,
            task_id: None,
            attempt: next_attempt(ctx, StageId::SpecReview, None, round),
            window_name: self.window_name(round, None),
        }
    }

    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit> {
        let round = current_round(ctx);
        if has_succeeded(ctx, StageId::SpecReview, None, round) {
            None
        } else {
            Some(WorkUnit {
                task_id: None,
                round,
                attempt: next_attempt(ctx, StageId::SpecReview, None, round),
            })
        }
    }

    fn phase_when_running(&self) -> Phase {
        Phase::Spec
    }

    fn next_phase_on_success(&self, _ctx: &StageCtx<'_>, _outcome: &SuccessOutcome) -> Phase {
        // Spec approval is now a PendingDecision, not a Phase variant; the
        // FSM consults it to either re-run another spec-review round or
        // move on to planning.
        Phase::Plan
    }

    fn artifact_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!("artifacts/spec-review-{round}.md"))]
    }

    fn restore_backups(&self, _round: u32) -> Vec<(PathBuf, PathBuf)> {
        Vec::new()
    }

    fn prompt_paths(&self, round: u32) -> Vec<PathBuf> {
        vec![PathBuf::from(format!("prompts/spec-review-{round}.md"))]
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
            phase: Phase::Spec,
            prior_runs: prior,
            pending_task_ids: pending,
            yolo: false,
            cheap: false,
        }
    }

    #[test]
    fn identity_and_window_match_legacy_launch() {
        let s = SpecReviewStage;
        assert_eq!(s.id(), StageId::SpecReview);
        assert_eq!(s.label(), "Spec Review");
        assert_eq!(s.window_name(1, None), "[Spec Review 1]");
        assert_eq!(s.window_name(2, None), "[Spec Review 2]");
        assert_eq!(s.phase_when_running(), Phase::Spec);
    }

    #[test]
    fn paths_vary_with_round() {
        let s = SpecReviewStage;
        assert_eq!(
            s.artifact_paths(1),
            vec![PathBuf::from("artifacts/spec-review-1.md")]
        );
        assert_eq!(
            s.artifact_paths(3),
            vec![PathBuf::from("artifacts/spec-review-3.md")]
        );
        assert_eq!(
            s.prompt_paths(2),
            vec![PathBuf::from("prompts/spec-review-2.md")]
        );
        assert!(s.restore_backups(1).is_empty());
    }

    #[test]
    fn current_round_advances_after_a_done_run() {
        let s = SpecReviewStage;
        assert_eq!(
            s.next_pending_work(&mk_ctx(&[], &[])),
            Some(WorkUnit {
                task_id: None,
                round: 1,
                attempt: 1
            })
        );
        let prior = [RunHistoryEntry {
            stage_id: StageId::SpecReview,
            task_id: None,
            round: 1,
            attempt: 1,
            outcome: Some(Outcome::Done),
        }];
        // Once round 1 is Done, the stage queues round 2.
        assert_eq!(
            s.next_pending_work(&mk_ctx(&prior, &[])),
            Some(WorkUnit {
                task_id: None,
                round: 2,
                attempt: 1
            })
        );
    }

    #[test]
    fn build_spec_carries_stage_id_and_window() {
        let s = SpecReviewStage;
        let spec = s.build_spec(&mk_ctx(&[], &[]));
        assert_eq!(spec.stage_id, StageId::SpecReview);
        assert_eq!(spec.window_name, "[Spec Review 1]");
    }
}
