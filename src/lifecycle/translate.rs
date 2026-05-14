//! Translation helpers between the legacy 24-variant
//! [`crate::state::Phase`] and the slim
//! [`super::Phase`] introduced for the lifecycle FSM cutover.
//!
//! Lives alongside the legacy enum during the multi-step cutover (Step 5a
//! wires the slim phase onto `App` as a derived projection; later steps
//! delete the legacy variants once their last consumer is gone).
use super::phase::Phase as SlimPhase;
use crate::state::Phase as LegacyPhase;

/// Project a legacy 24-variant phase down to the slim lifecycle phase.
///
/// The mapping mirrors what each `Stage` impl's `phase_when_running()`
/// declares for the lifecycle module's stages:
///
/// | Legacy variant(s)                                                  | Slim phase           |
/// |--------------------------------------------------------------------|----------------------|
/// | `IdeaInput`, `BrainstormRunning`                                   | `Idea`               |
/// | `SpecReviewRunning`, `SpecReviewPaused`                            | `Spec`               |
/// | `PlanningRunning`, `PlanReviewRunning`, `PlanReviewPaused`,        |                      |
/// | `ShardingRunning`, `RepoStateUpdateRunning`, `WaitingToImplement`, |                      |
/// | `SkipToImplPending`                                                | `Plan`               |
/// | `ImplementationRound(r)`, `BuilderRecovery(r)`,                    |                      |
/// | `BuilderRecoveryPlanReview(r)`, `BuilderRecoverySharding(r)`       | `Implementation(r)`  |
/// | `ReviewRound(r)`, `Simplification(r)`                              | `Review(r)`          |
/// | `FinalValidation(_)`, `Dreaming(_)`, `DreamingPending`             | `Finalization`       |
/// | `Done`                                                             | `Done`               |
/// | `Cancelled`                                                        | `Cancelled`          |
/// | `BlockedNeedsUser`, `GitGuardPending`                              | `Plan` (see note)    |
///
/// `BlockedNeedsUser` and `GitGuardPending` are modal/pending decisions
/// rather than pipeline positions. The slim phase has no place for them,
/// so we route them to `Phase::Plan` since the git-guard modal fires only
/// during the Plan/Implementation arc; the consumer treats this as the
/// last-known-non-pending value pending Step 5b/5c's full cutover.
pub fn slim_phase_for(old: &LegacyPhase) -> SlimPhase {
    match *old {
        LegacyPhase::IdeaInput | LegacyPhase::BrainstormRunning => SlimPhase::Idea,
        LegacyPhase::SpecReviewRunning | LegacyPhase::SpecReviewPaused => SlimPhase::Spec,
        LegacyPhase::PlanningRunning
        | LegacyPhase::PlanReviewRunning
        | LegacyPhase::PlanReviewPaused
        | LegacyPhase::ShardingRunning
        | LegacyPhase::RepoStateUpdateRunning
        | LegacyPhase::WaitingToImplement
        | LegacyPhase::SkipToImplPending => SlimPhase::Plan,
        LegacyPhase::ImplementationRound(r)
        | LegacyPhase::BuilderRecovery(r)
        | LegacyPhase::BuilderRecoveryPlanReview(r)
        | LegacyPhase::BuilderRecoverySharding(r) => SlimPhase::Implementation(r),
        LegacyPhase::ReviewRound(r) | LegacyPhase::Simplification(r) => SlimPhase::Review(r),
        LegacyPhase::FinalValidation(_)
        | LegacyPhase::Dreaming(_)
        | LegacyPhase::DreamingPending => SlimPhase::Finalization,
        LegacyPhase::Done => SlimPhase::Done,
        LegacyPhase::Cancelled => SlimPhase::Cancelled,
        // Modal/pending decisions are not pipeline positions; the slim
        // phase has no equivalent so we route them through Plan (the
        // last-known-non-pending value in the legacy pipeline arc that
        // raises these modals). 5b/5c remove this surrogate once the
        // legacy phase enum's consumers are gone.
        LegacyPhase::BlockedNeedsUser | LegacyPhase::GitGuardPending => SlimPhase::Plan,
    }
}

/// Best-effort lifecycle [`StageId`](super::stage_id::StageId) for a
/// legacy run record's `stage` string and `window_name` discriminators.
///
/// Mirrors [`crate::app::RetryLaunch::for_run`] so the FSM-mirroring shim
/// can synthesize a [`super::StageSpec`] from the existing `RunRecord`
/// without rebuilding the per-stage logic. Recovery sub-stages share the
/// `stage == "recovery"` string so we key off the human-readable window
/// label to preserve fidelity.
pub fn stage_id_for_run(stage: &str, window_name: &str) -> Option<super::StageId> {
    if window_name.contains("[Recovery Plan Review]") {
        return Some(super::StageId::RecoveryPlanReview);
    }
    if window_name.contains("[Recovery Sharding]") {
        return Some(super::StageId::RecoverySharding);
    }
    Some(match stage {
        "brainstorm" => super::StageId::Brainstorm,
        "spec-review" => super::StageId::SpecReview,
        "planning" => super::StageId::Planning,
        "plan-review" => super::StageId::PlanReview,
        "sharding" => super::StageId::Sharding,
        "recovery" => super::StageId::Recovery,
        "coder" => super::StageId::Coder,
        "reviewer" => super::StageId::Reviewer,
        "final-validation" => super::StageId::FinalValidation,
        "simplifier" => super::StageId::Simplification,
        "dreaming" => super::StageId::Dreaming,
        "repo-state-update" => super::StageId::RepoStateUpdate,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_idea_maps_to_slim_idea() {
        assert_eq!(slim_phase_for(&LegacyPhase::IdeaInput), SlimPhase::Idea);
        assert_eq!(
            slim_phase_for(&LegacyPhase::BrainstormRunning),
            SlimPhase::Idea
        );
    }

    #[test]
    fn legacy_implementation_round_preserved() {
        assert_eq!(
            slim_phase_for(&LegacyPhase::ImplementationRound(3)),
            SlimPhase::Implementation(3)
        );
        assert_eq!(
            slim_phase_for(&LegacyPhase::BuilderRecovery(2)),
            SlimPhase::Implementation(2)
        );
    }

    #[test]
    fn legacy_review_round_includes_simplification() {
        assert_eq!(
            slim_phase_for(&LegacyPhase::ReviewRound(4)),
            SlimPhase::Review(4)
        );
        assert_eq!(
            slim_phase_for(&LegacyPhase::Simplification(4)),
            SlimPhase::Review(4)
        );
    }

    #[test]
    fn legacy_finalization_arc_collapses() {
        assert_eq!(
            slim_phase_for(&LegacyPhase::FinalValidation(1)),
            SlimPhase::Finalization
        );
        assert_eq!(
            slim_phase_for(&LegacyPhase::Dreaming(1)),
            SlimPhase::Finalization
        );
        assert_eq!(
            slim_phase_for(&LegacyPhase::DreamingPending),
            SlimPhase::Finalization
        );
    }

    #[test]
    fn legacy_terminals_preserved() {
        assert_eq!(slim_phase_for(&LegacyPhase::Done), SlimPhase::Done);
        assert_eq!(
            slim_phase_for(&LegacyPhase::Cancelled),
            SlimPhase::Cancelled
        );
    }

    #[test]
    fn legacy_pending_modals_route_to_plan() {
        // Documented surrogate — see module docs.
        assert_eq!(
            slim_phase_for(&LegacyPhase::BlockedNeedsUser),
            SlimPhase::Plan
        );
        assert_eq!(
            slim_phase_for(&LegacyPhase::GitGuardPending),
            SlimPhase::Plan
        );
    }

    #[test]
    fn stage_id_for_run_handles_recovery_subwindows() {
        use super::super::StageId;
        assert_eq!(
            stage_id_for_run("recovery", "[Recovery Plan Review]"),
            Some(StageId::RecoveryPlanReview)
        );
        assert_eq!(
            stage_id_for_run("recovery", "[Recovery Sharding] r1"),
            Some(StageId::RecoverySharding)
        );
        assert_eq!(
            stage_id_for_run("recovery", "[Recovery]"),
            Some(StageId::Recovery)
        );
    }

    #[test]
    fn stage_id_for_run_maps_stage_strings() {
        use super::super::StageId;
        assert_eq!(stage_id_for_run("coder", "[Builder r1]"), Some(StageId::Coder));
        assert_eq!(
            stage_id_for_run("simplifier", "[Simplifier]"),
            Some(StageId::Simplification)
        );
        assert_eq!(stage_id_for_run("unknown-stage", ""), None);
    }
}
