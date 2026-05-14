//! Translation helpers between the legacy 24-variant
//! [`crate::state::Phase`] and the slim
//! [`super::Phase`] introduced for the lifecycle FSM cutover.
//!
//! Lives alongside the legacy enum during the multi-step cutover (Step 5a
//! wires the slim phase onto `App` as a derived projection; later steps
//! delete the legacy variants once their last consumer is gone).
use super::phase::Phase as SlimPhase;
use crate::logic::rules::retry_phase_for_stage;
use crate::state::{Phase as LegacyPhase, SessionState};

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

/// Slim [`Phase`] to rewind to when the operator retries a specific task.
///
/// Mirrors today's `App::retry_task` round-derivation: pick the highest
/// round number observed for the task across `agent_runs`, fall back to the
/// current legacy phase's embedded round, and default to round 1.
/// Always returns `Phase::Implementation(round)` because tasks only exist
/// inside an implementation round (their coder/reviewer chain shares the
/// same `task_id`).
pub fn slim_phase_for_task_retry(task_id: u32, state: &SessionState) -> SlimPhase {
    let max_round = state
        .agent_runs
        .iter()
        .filter(|run| run.task_id == Some(task_id))
        .map(|run| run.round)
        .max();
    let phase_round = match state.current_phase {
        LegacyPhase::ImplementationRound(r) | LegacyPhase::ReviewRound(r) => Some(r),
        LegacyPhase::BuilderRecovery(r)
        | LegacyPhase::BuilderRecoveryPlanReview(r)
        | LegacyPhase::BuilderRecoverySharding(r) => Some(r),
        _ => None,
    };
    let round = max_round.or(phase_round).unwrap_or(1);
    SlimPhase::Implementation(round)
}

/// Slim [`Phase`] to rewind to when the operator retries a stage by name.
///
/// Maps the stage string through the legacy
/// [`retry_phase_for_stage`](crate::logic::rules::retry_phase_for_stage) and
/// projects the result down via [`slim_phase_for`]. Returns `Phase::Plan`
/// for stages whose retry target isn't in the legacy table — the rewind is
/// still valid (Plan is the broadest legitimate target for non-implementation
/// stages) and the lane-gate check in the App still enforces the invariant.
pub fn slim_phase_for_stage_retry(stage: &str) -> SlimPhase {
    match retry_phase_for_stage(stage) {
        Some(legacy) => slim_phase_for(&legacy),
        None => SlimPhase::Plan,
    }
}

/// Inverse of [`slim_phase_for`]: pick a representative legacy phase to
/// land on when the operator rewinds to `target`.
///
/// The slim → legacy map is many-to-one, so this picks the "running" phase
/// for each slot so the legacy launch/auto-launch path can take over.
pub fn slim_to_old_phase(target: SlimPhase) -> LegacyPhase {
    match target {
        SlimPhase::Idea => LegacyPhase::IdeaInput,
        SlimPhase::Spec => LegacyPhase::SpecReviewRunning,
        // Scheduler-side fast-forward picks PlanReviewRunning if plan.md
        // already exists; landing on PlanningRunning is the safe default.
        SlimPhase::Plan => LegacyPhase::PlanningRunning,
        SlimPhase::Implementation(r) => LegacyPhase::ImplementationRound(r),
        SlimPhase::Review(r) => LegacyPhase::ReviewRound(r),
        SlimPhase::Finalization => LegacyPhase::FinalValidation(1),
        SlimPhase::Done => LegacyPhase::Done,
        SlimPhase::Cancelled => LegacyPhase::Cancelled,
    }
}

/// Best-effort lifecycle [`StageId`](super::stage_id::StageId) for a
/// legacy run record's `stage` string and `window_name` discriminators.
///
/// Synthesizes a [`super::StageId`] from the existing `RunRecord` fields.
/// Recovery sub-stages share the `stage == "recovery"` string, so we key off
/// the human-readable window label to preserve fidelity.
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
    fn task_retry_picks_max_round_then_phase_then_one() {
        use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};
        let mut state = SessionState::new("test".to_string());
        // No runs and no implementation-arc phase → round 1.
        state.current_phase = LegacyPhase::IdeaInput;
        assert_eq!(
            slim_phase_for_task_retry(7, &state),
            SlimPhase::Implementation(1)
        );
        // Phase fallback when no runs match the task.
        state.current_phase = LegacyPhase::ImplementationRound(3);
        assert_eq!(
            slim_phase_for_task_retry(7, &state),
            SlimPhase::Implementation(3)
        );
        // Run history takes precedence — round 5 > phase round 3.
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 5,
            attempt: 1,
            model: String::new(),
            subscription_label: String::new(),
            window_name: "[Round 5 Coder] task 7".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Done,
            error: None,
            effort: crate::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        assert_eq!(
            slim_phase_for_task_retry(7, &state),
            SlimPhase::Implementation(5)
        );
    }

    #[test]
    fn stage_retry_routes_through_legacy_table() {
        assert_eq!(slim_phase_for_stage_retry("brainstorm"), SlimPhase::Idea);
        assert_eq!(slim_phase_for_stage_retry("spec-review"), SlimPhase::Spec);
        assert_eq!(slim_phase_for_stage_retry("planning"), SlimPhase::Plan);
        assert_eq!(slim_phase_for_stage_retry("plan-review"), SlimPhase::Plan);
        // Sharding retry maps to WaitingToImplement, which slim_phase_for
        // surfaces as Plan.
        assert_eq!(slim_phase_for_stage_retry("sharding"), SlimPhase::Plan);
        // Unknown stage falls back to Plan.
        assert_eq!(slim_phase_for_stage_retry("unknown"), SlimPhase::Plan);
    }

    #[test]
    fn slim_to_old_phase_inverse_lands_on_running_variant() {
        assert_eq!(slim_to_old_phase(SlimPhase::Idea), LegacyPhase::IdeaInput);
        assert_eq!(
            slim_to_old_phase(SlimPhase::Spec),
            LegacyPhase::SpecReviewRunning
        );
        assert_eq!(
            slim_to_old_phase(SlimPhase::Plan),
            LegacyPhase::PlanningRunning
        );
        assert_eq!(
            slim_to_old_phase(SlimPhase::Implementation(4)),
            LegacyPhase::ImplementationRound(4)
        );
        assert_eq!(
            slim_to_old_phase(SlimPhase::Review(2)),
            LegacyPhase::ReviewRound(2)
        );
        assert_eq!(
            slim_to_old_phase(SlimPhase::Finalization),
            LegacyPhase::FinalValidation(1)
        );
        assert_eq!(slim_to_old_phase(SlimPhase::Done), LegacyPhase::Done);
        assert_eq!(
            slim_to_old_phase(SlimPhase::Cancelled),
            LegacyPhase::Cancelled
        );
    }

    #[test]
    fn stage_id_for_run_maps_stage_strings() {
        use super::super::StageId;
        assert_eq!(
            stage_id_for_run("coder", "[Builder r1]"),
            Some(StageId::Coder)
        );
        assert_eq!(
            stage_id_for_run("simplifier", "[Simplifier]"),
            Some(StageId::Simplification)
        );
        assert_eq!(stage_id_for_run("unknown-stage", ""), None);
    }
}
