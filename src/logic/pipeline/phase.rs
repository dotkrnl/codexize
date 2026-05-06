use crate::artifacts::ArtifactKind;
use serde::{Deserialize, Serialize};

// `EnumString` is intentionally not derived: no caller parses phase names back
// into `Phase`, and the parameterized variants would require runtime-format
// parsing that strum cannot generate.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    strum::Display,
    strum::IntoStaticStr,
    strum::EnumDiscriminants,
)]
#[strum_discriminants(name(PhaseKind))]
pub enum Phase {
    #[strum(to_string = "Idea Input")]
    IdeaInput,
    #[strum(to_string = "Brainstorming")]
    BrainstormRunning,
    #[strum(to_string = "Spec Review")]
    SpecReviewRunning,
    #[strum(to_string = "Spec Review")]
    SpecReviewPaused,
    #[strum(to_string = "Planning")]
    PlanningRunning,
    #[strum(to_string = "Plan Review")]
    PlanReviewRunning,
    #[strum(to_string = "Plan Review")]
    PlanReviewPaused,
    #[strum(to_string = "Sharding")]
    ShardingRunning,
    #[strum(to_string = "Skip Confirmation")]
    SkipToImplPending, // New phase
    /// Coder agent is working on the current task in round N.
    #[strum(to_string = "Implementation Round {0}")]
    ImplementationRound(u32),
    /// Reviewer agent is checking the current task's work in round N.
    #[strum(to_string = "Review Round {0}")]
    ReviewRound(u32),
    /// Builder-only recovery stage that repairs artifacts and reconciles queue state.
    ///
    /// The stored round is the builder round that triggered recovery; successful recovery
    /// resumes into `BuilderRecoveryPlanReview` of the same round.
    #[strum(to_string = "Builder Recovery")]
    BuilderRecovery(u32),
    /// Non-interactive plan review inserted after a builder recovery stage completes.
    /// Verifies the recovered spec/plan is coherent before sharding.
    #[strum(to_string = "Recovery Plan Review")]
    BuilderRecoveryPlanReview(u32),
    /// Recovery-mode re-sharding inserted after a successful recovery plan review.
    /// Regenerates the task queue from the recovered spec/plan.
    #[strum(to_string = "Recovery Sharding")]
    BuilderRecoverySharding(u32),
    /// Interactive non-coder run advanced HEAD under `GuardMode::AskOperator`;
    /// the modal is up and the operator must choose reset or keep before the
    /// run can finalize.
    #[strum(to_string = "Guard Decision")]
    GitGuardPending,
    /// Final goal validation runs once per queue-empty pre-`Done` boundary.
    /// The stored round is the coder round whose work is being validated.
    #[strum(to_string = "Final Validation Round {0}")]
    FinalValidation(u32),
    /// Behavior-preserving cleanup pass between loop convergence and FinalValidation.
    /// The stored round matches the coder round whose work is being simplified.
    #[strum(to_string = "Simplification Round {0}")]
    Simplification(u32),
    #[strum(to_string = "Done")]
    Done,
    #[strum(to_string = "Blocked")]
    BlockedNeedsUser,
}

#[derive(Debug, Clone, Copy)]
struct TransitionEdge {
    from: PhaseKind,
    to: PhaseKind,
    guard: RoundGuard,
}

impl TransitionEdge {
    const fn new(from: PhaseKind, to: PhaseKind) -> Self {
        Self {
            from,
            to,
            guard: RoundGuard::Any,
        }
    }

    const fn guarded(from: PhaseKind, to: PhaseKind, guard: RoundGuard) -> Self {
        Self { from, to, guard }
    }

    fn allows(self, from: &Phase, to: &Phase) -> bool {
        self.from == PhaseKind::from(from)
            && self.to == PhaseKind::from(to)
            && self.guard.allows(from.round(), to.round())
    }
}

#[derive(Debug, Clone, Copy)]
enum RoundGuard {
    Any,
    Same,
    SameNonZero,
    ToNext,
    ToPrevious,
    ToRound(u32),
    FromAtMost(u32),
}

impl RoundGuard {
    fn allows(self, from: Option<u32>, to: Option<u32>) -> bool {
        match self {
            Self::Any => true,
            Self::Same => from.zip(to).is_some_and(|(from, to)| from == to),
            Self::SameNonZero => from
                .zip(to)
                .is_some_and(|(from, to)| from == to && from > 0),
            Self::ToNext => from
                .and_then(|from| from.checked_add(1))
                .zip(to)
                .is_some_and(|(expected, to)| expected == to),
            Self::ToPrevious => to
                .and_then(|to| to.checked_add(1))
                .zip(from)
                .is_some_and(|(expected, from)| expected == from),
            Self::ToRound(round) => to == Some(round),
            Self::FromAtMost(max) => from.is_some_and(|from| from <= max),
        }
    }
}

use PhaseKind as P;

// A table stays clearer than statig/rust-fsm here because most parameterized
// edges are simple round guards rather than state-entry/exit actions.
const TRANSITION_EDGES: &[TransitionEdge] = &[
    TransitionEdge::new(P::IdeaInput, P::BrainstormRunning),
    TransitionEdge::new(P::BrainstormRunning, P::SpecReviewRunning),
    TransitionEdge::new(P::BrainstormRunning, P::BlockedNeedsUser),
    TransitionEdge::new(P::BrainstormRunning, P::SkipToImplPending),
    TransitionEdge::new(P::SpecReviewRunning, P::SpecReviewPaused),
    TransitionEdge::new(P::SpecReviewRunning, P::PlanningRunning),
    TransitionEdge::new(P::SpecReviewRunning, P::BlockedNeedsUser),
    TransitionEdge::new(P::SpecReviewPaused, P::SpecReviewRunning),
    TransitionEdge::new(P::SpecReviewPaused, P::PlanningRunning),
    TransitionEdge::new(P::SpecReviewPaused, P::BlockedNeedsUser),
    TransitionEdge::new(P::SkipToImplPending, P::ImplementationRound),
    TransitionEdge::new(P::SkipToImplPending, P::SpecReviewRunning),
    TransitionEdge::new(P::SkipToImplPending, P::BlockedNeedsUser),
    TransitionEdge::new(P::SkipToImplPending, P::Done),
    TransitionEdge::new(P::SkipToImplPending, P::BrainstormRunning),
    TransitionEdge::new(P::PlanningRunning, P::PlanReviewRunning),
    TransitionEdge::new(P::PlanningRunning, P::ShardingRunning),
    TransitionEdge::new(P::PlanningRunning, P::BlockedNeedsUser),
    TransitionEdge::new(P::PlanReviewRunning, P::ShardingRunning),
    TransitionEdge::new(P::PlanReviewRunning, P::BlockedNeedsUser),
    TransitionEdge::new(P::PlanReviewPaused, P::PlanReviewRunning),
    TransitionEdge::new(P::PlanReviewPaused, P::ShardingRunning),
    TransitionEdge::new(P::PlanReviewPaused, P::BlockedNeedsUser),
    TransitionEdge::new(P::BlockedNeedsUser, P::PlanReviewRunning),
    TransitionEdge::guarded(
        P::ShardingRunning,
        P::ImplementationRound,
        RoundGuard::ToRound(1),
    ),
    TransitionEdge::new(P::ShardingRunning, P::BlockedNeedsUser),
    TransitionEdge::guarded(P::ImplementationRound, P::ReviewRound, RoundGuard::Same),
    TransitionEdge::guarded(P::ImplementationRound, P::BuilderRecovery, RoundGuard::Same),
    TransitionEdge::new(P::ImplementationRound, P::BlockedNeedsUser),
    TransitionEdge::guarded(P::ReviewRound, P::ImplementationRound, RoundGuard::ToNext),
    TransitionEdge::new(P::ReviewRound, P::Done),
    TransitionEdge::new(P::ReviewRound, P::BlockedNeedsUser),
    TransitionEdge::guarded(P::ReviewRound, P::BuilderRecovery, RoundGuard::Same),
    TransitionEdge::guarded(
        P::BuilderRecovery,
        P::ImplementationRound,
        RoundGuard::ToNext,
    ),
    TransitionEdge::guarded(
        P::BuilderRecovery,
        P::BuilderRecoveryPlanReview,
        RoundGuard::Same,
    ),
    TransitionEdge::guarded(
        P::BuilderRecovery,
        P::BuilderRecoverySharding,
        RoundGuard::Same,
    ),
    TransitionEdge::new(P::BuilderRecovery, P::BlockedNeedsUser),
    TransitionEdge::guarded(
        P::BuilderRecoveryPlanReview,
        P::BuilderRecoverySharding,
        RoundGuard::Same,
    ),
    TransitionEdge::guarded(
        P::BuilderRecoveryPlanReview,
        P::BuilderRecovery,
        RoundGuard::Same,
    ),
    TransitionEdge::new(P::BuilderRecoveryPlanReview, P::BlockedNeedsUser),
    TransitionEdge::guarded(
        P::BuilderRecoverySharding,
        P::ImplementationRound,
        RoundGuard::ToNext,
    ),
    TransitionEdge::new(P::BuilderRecoverySharding, P::BlockedNeedsUser),
    TransitionEdge::new(P::BlockedNeedsUser, P::BrainstormRunning),
    TransitionEdge::new(P::BlockedNeedsUser, P::SpecReviewRunning),
    TransitionEdge::new(P::BlockedNeedsUser, P::PlanningRunning),
    TransitionEdge::new(P::BlockedNeedsUser, P::ShardingRunning),
    TransitionEdge::new(P::BlockedNeedsUser, P::ImplementationRound),
    TransitionEdge::new(P::BlockedNeedsUser, P::ReviewRound),
    TransitionEdge::new(P::BlockedNeedsUser, P::BuilderRecovery),
    TransitionEdge::new(P::BrainstormRunning, P::GitGuardPending),
    TransitionEdge::new(P::PlanningRunning, P::GitGuardPending),
    TransitionEdge::new(P::BuilderRecovery, P::GitGuardPending),
    TransitionEdge::new(P::GitGuardPending, P::BlockedNeedsUser),
    TransitionEdge::new(P::GitGuardPending, P::Done),
    TransitionEdge::new(P::GitGuardPending, P::SpecReviewRunning),
    TransitionEdge::new(P::GitGuardPending, P::SkipToImplPending),
    TransitionEdge::new(P::GitGuardPending, P::PlanReviewRunning),
    TransitionEdge::new(P::GitGuardPending, P::BuilderRecoveryPlanReview),
    TransitionEdge::new(P::FinalValidation, P::Done),
    TransitionEdge::guarded(
        P::FinalValidation,
        P::ImplementationRound,
        RoundGuard::ToNext,
    ),
    TransitionEdge::new(P::FinalValidation, P::BlockedNeedsUser),
    TransitionEdge::guarded(P::ReviewRound, P::Simplification, RoundGuard::Same),
    TransitionEdge::guarded(
        P::ImplementationRound,
        P::Simplification,
        RoundGuard::ToRound(1),
    ),
    TransitionEdge::new(P::BlockedNeedsUser, P::Simplification),
    TransitionEdge::guarded(P::Simplification, P::FinalValidation, RoundGuard::Same),
    TransitionEdge::new(P::Simplification, P::BlockedNeedsUser),
    TransitionEdge::new(P::BlockedNeedsUser, P::FinalValidation),
    TransitionEdge::new(P::BlockedNeedsUser, P::Done),
    TransitionEdge::new(P::BrainstormRunning, P::IdeaInput),
    TransitionEdge::new(P::SpecReviewRunning, P::BrainstormRunning),
    TransitionEdge::new(P::SpecReviewPaused, P::BrainstormRunning),
    TransitionEdge::new(P::PlanningRunning, P::SpecReviewRunning),
    TransitionEdge::new(P::ShardingRunning, P::PlanReviewRunning),
    TransitionEdge::new(P::PlanReviewRunning, P::PlanningRunning),
    TransitionEdge::new(P::PlanReviewRunning, P::PlanReviewPaused),
    TransitionEdge::new(P::PlanReviewPaused, P::PlanningRunning),
    TransitionEdge::guarded(
        P::ImplementationRound,
        P::ShardingRunning,
        RoundGuard::FromAtMost(1),
    ),
    TransitionEdge::guarded(
        P::ImplementationRound,
        P::BrainstormRunning,
        RoundGuard::FromAtMost(1),
    ),
    TransitionEdge::guarded(
        P::ImplementationRound,
        P::ReviewRound,
        RoundGuard::ToPrevious,
    ),
    TransitionEdge::guarded(P::ReviewRound, P::ImplementationRound, RoundGuard::Same),
    TransitionEdge::guarded(P::FinalValidation, P::ReviewRound, RoundGuard::SameNonZero),
    TransitionEdge::guarded(
        P::FinalValidation,
        P::ImplementationRound,
        RoundGuard::ToRound(1),
    ),
    TransitionEdge::guarded(P::Simplification, P::ReviewRound, RoundGuard::SameNonZero),
    TransitionEdge::guarded(
        P::Simplification,
        P::ImplementationRound,
        RoundGuard::ToRound(1),
    ),
];

impl Phase {
    fn round(self) -> Option<u32> {
        match self {
            Phase::ImplementationRound(round)
            | Phase::ReviewRound(round)
            | Phase::BuilderRecovery(round)
            | Phase::BuilderRecoveryPlanReview(round)
            | Phase::BuilderRecoverySharding(round)
            | Phase::FinalValidation(round)
            | Phase::Simplification(round) => Some(round),
            _ => None,
        }
    }

    /// Short, TUI-facing label used by the dashboard/picker. Spec §3.2.4
    /// proposes folding `label()` into Display, but several variants here
    /// (`Round N Coder`, `Final Validation` without round, `Simplification`
    /// without round) are deliberately shorter than their `Display` form to
    /// fit the user-visible chrome we are not allowed to change. Keep this
    /// helper when reconciling the spec line.
    pub fn label(&self) -> String {
        match self {
            Phase::IdeaInput => "Idea Input".to_string(),
            Phase::BrainstormRunning => "Brainstorming".to_string(),
            Phase::SpecReviewRunning => "Spec Review".to_string(),
            Phase::SpecReviewPaused => "Spec Review".to_string(),
            Phase::PlanningRunning => "Planning".to_string(),
            Phase::PlanReviewRunning => "Plan Review".to_string(),
            Phase::PlanReviewPaused => "Plan Review".to_string(),
            Phase::ShardingRunning => "Sharding".to_string(),
            Phase::ImplementationRound(r) => format!("Round {r} Coder"),
            Phase::ReviewRound(r) => format!("Round {r} Reviewer"),
            Phase::BuilderRecovery(_) => "Builder Recovery".to_string(),
            Phase::BuilderRecoveryPlanReview(_) => "Recovery Plan Review".to_string(),
            Phase::BuilderRecoverySharding(_) => "Recovery Sharding".to_string(),
            Phase::Done => "Done".to_string(),
            Phase::BlockedNeedsUser => "Blocked".to_string(),
            Phase::SkipToImplPending => "Skip Confirmation".to_string(),
            Phase::GitGuardPending => "Guard Decision".to_string(),
            Phase::FinalValidation(_) => "Final Validation".to_string(),
            Phase::Simplification(_) => "Simplification".to_string(),
        }
    }

    /// Returns true if a transition from `self` to `target` is valid.
    pub fn can_transition_to(&self, target: &Phase) -> bool {
        TRANSITION_EDGES
            .iter()
            .any(|edge| edge.allows(self, target))
    }

    /// Artifacts that should exist before entering this phase.
    #[allow(dead_code)]
    pub fn required_artifacts(&self) -> Vec<ArtifactKind> {
        match self {
            Phase::SpecReviewRunning => vec![ArtifactKind::Spec],
            Phase::PlanningRunning => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::PlanReviewRunning => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::PlanReviewPaused => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::ShardingRunning => vec![ArtifactKind::Plan],
            Phase::ImplementationRound(_) => vec![ArtifactKind::Plan, ArtifactKind::Tasks],
            Phase::ReviewRound(_) => vec![ArtifactKind::CodeReview],
            Phase::BuilderRecovery(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::BuilderRecoveryPlanReview(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::BuilderRecoverySharding(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::SkipToImplPending => vec![], // No artifacts required for this phase itself
            Phase::GitGuardPending => vec![],
            Phase::FinalValidation(_) => vec![ArtifactKind::Spec],
            Phase::Simplification(_) => vec![ArtifactKind::Spec],
            _ => vec![],
        }
    }

    /// Human-readable description of what happens in this phase.
    #[allow(dead_code)]
    pub fn description(&self) -> &'static str {
        match self {
            Phase::IdeaInput => "Waiting for user input",
            Phase::BrainstormRunning => "AI agent generating specification",
            Phase::SpecReviewRunning => "AI agent reviewing specification",
            Phase::SpecReviewPaused => "Specification review paused",
            Phase::PlanningRunning => "AI agent creating implementation plan",
            Phase::PlanReviewRunning => "AI agent reviewing implementation plan",
            Phase::PlanReviewPaused => "Plan review paused",
            Phase::ShardingRunning => "Splitting plan into actionable tasks",
            Phase::ImplementationRound(_) => "AI agent implementing code",
            Phase::ReviewRound(_) => "AI agent reviewing implementation",
            Phase::BuilderRecovery(_) => "AI agent repairing builder artifacts",
            Phase::BuilderRecoveryPlanReview(_) => "Validating recovered spec and plan",
            Phase::BuilderRecoverySharding(_) => "Regenerating tasks from recovered plan",
            Phase::Done => "Run completed successfully",
            Phase::BlockedNeedsUser => "Blocked - requires user intervention",
            Phase::SkipToImplPending => "Awaiting user confirmation to skip to implementation",
            Phase::GitGuardPending => "Awaiting git guard decision",
            Phase::FinalValidation(_) => {
                "AI agent verifying the original goal against the live workspace"
            }
            Phase::Simplification(_) => {
                "AI agent applying behavior-preserving simplifications before final validation"
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::Phase;

    #[test]
    fn plan_review_forward_transitions() {
        assert!(Phase::PlanningRunning.can_transition_to(&Phase::PlanReviewRunning));
        assert!(Phase::PlanningRunning.can_transition_to(&Phase::ShardingRunning));
        assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::PlanReviewPaused));
        assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::ShardingRunning));
        assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::BlockedNeedsUser));
        assert!(Phase::PlanReviewPaused.can_transition_to(&Phase::PlanReviewRunning));
        assert!(Phase::PlanReviewPaused.can_transition_to(&Phase::ShardingRunning));
        assert!(Phase::PlanReviewPaused.can_transition_to(&Phase::BlockedNeedsUser));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::PlanReviewRunning));
    }

    #[test]
    fn plan_review_backward_transitions() {
        assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::PlanningRunning));
        assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::PlanReviewPaused));
        assert!(Phase::PlanReviewPaused.can_transition_to(&Phase::PlanningRunning));
        assert!(Phase::ShardingRunning.can_transition_to(&Phase::PlanReviewRunning));
    }

    #[test]
    fn plan_review_invalid_transitions() {
        assert!(!Phase::PlanReviewPaused.can_transition_to(&Phase::PlanReviewPaused));
        assert!(!Phase::PlanReviewRunning.can_transition_to(&Phase::PlanReviewRunning));
        assert!(!Phase::PlanReviewPaused.can_transition_to(&Phase::Done));
        assert!(!Phase::IdeaInput.can_transition_to(&Phase::PlanReviewRunning));
    }

    #[test]
    fn sharding_no_longer_goes_back_to_planning() {
        assert!(!Phase::ShardingRunning.can_transition_to(&Phase::PlanningRunning));
    }

    #[test]
    fn plan_review_labels() {
        assert_eq!(Phase::PlanReviewRunning.label(), "Plan Review");
        assert_eq!(Phase::PlanReviewPaused.label(), "Plan Review");
        assert_eq!(format!("{}", Phase::PlanReviewRunning), "Plan Review");
        assert_eq!(format!("{}", Phase::PlanReviewPaused), "Plan Review");
    }

    #[test]
    fn builder_recovery_transitions() {
        assert!(Phase::ImplementationRound(3).can_transition_to(&Phase::BuilderRecovery(3)));
        assert!(Phase::ReviewRound(3).can_transition_to(&Phase::BuilderRecovery(3)));
        assert!(Phase::BuilderRecovery(3).can_transition_to(&Phase::ImplementationRound(4)));
        assert!(Phase::BuilderRecovery(3).can_transition_to(&Phase::BuilderRecoverySharding(3)));
        assert!(Phase::BuilderRecovery(3).can_transition_to(&Phase::BlockedNeedsUser));
        assert_eq!(Phase::BuilderRecovery(1).label(), "Builder Recovery");
        assert_eq!(
            Phase::BuilderRecovery(1).description(),
            "AI agent repairing builder artifacts"
        );
    }

    #[test]
    fn skip_to_impl_pending_transitions() {
        assert!(Phase::BrainstormRunning.can_transition_to(&Phase::SkipToImplPending));
        assert!(Phase::SkipToImplPending.can_transition_to(&Phase::ImplementationRound(1)));
        assert!(Phase::SkipToImplPending.can_transition_to(&Phase::SpecReviewRunning));
        assert!(Phase::SkipToImplPending.can_transition_to(&Phase::BlockedNeedsUser));

        // Negative tests
        assert!(!Phase::SpecReviewRunning.can_transition_to(&Phase::SkipToImplPending));
        assert!(!Phase::PlanningRunning.can_transition_to(&Phase::SkipToImplPending));
        assert!(!Phase::SkipToImplPending.can_transition_to(&Phase::PlanningRunning));
    }

    #[test]
    fn git_guard_pending_inbound_transitions() {
        assert!(Phase::BrainstormRunning.can_transition_to(&Phase::GitGuardPending));
        assert!(Phase::PlanningRunning.can_transition_to(&Phase::GitGuardPending));
        assert!(Phase::BuilderRecovery(2).can_transition_to(&Phase::GitGuardPending));

        // Phases that never launch with AskOperator must not enter the
        // pending state directly.
        assert!(!Phase::SpecReviewRunning.can_transition_to(&Phase::GitGuardPending));
        assert!(!Phase::PlanReviewRunning.can_transition_to(&Phase::GitGuardPending));
        assert!(!Phase::ShardingRunning.can_transition_to(&Phase::GitGuardPending));
        assert!(!Phase::ImplementationRound(1).can_transition_to(&Phase::GitGuardPending));
        assert!(!Phase::ReviewRound(1).can_transition_to(&Phase::GitGuardPending));
    }

    #[test]
    fn git_guard_pending_outbound_transitions() {
        assert!(Phase::GitGuardPending.can_transition_to(&Phase::BlockedNeedsUser));
        assert!(Phase::GitGuardPending.can_transition_to(&Phase::Done));
        assert!(Phase::GitGuardPending.can_transition_to(&Phase::SpecReviewRunning));
        assert!(Phase::GitGuardPending.can_transition_to(&Phase::SkipToImplPending));
        assert!(Phase::GitGuardPending.can_transition_to(&Phase::PlanReviewRunning));
        assert!(Phase::GitGuardPending.can_transition_to(&Phase::BuilderRecoveryPlanReview(3)));

        // Negative cases — pending must not bypass into stages no
        // brainstorm/planning/recovery run could reach today.
        assert!(!Phase::GitGuardPending.can_transition_to(&Phase::ImplementationRound(1)));
        assert!(!Phase::GitGuardPending.can_transition_to(&Phase::ReviewRound(1)));
        assert!(!Phase::GitGuardPending.can_transition_to(&Phase::ShardingRunning));
        assert!(!Phase::GitGuardPending.can_transition_to(&Phase::IdeaInput));
        assert!(!Phase::GitGuardPending.can_transition_to(&Phase::GitGuardPending));
    }

    #[test]
    fn git_guard_pending_metadata() {
        assert_eq!(Phase::GitGuardPending.label(), "Guard Decision");
        assert_eq!(
            Phase::GitGuardPending.description(),
            "Awaiting git guard decision"
        );
        assert!(Phase::GitGuardPending.required_artifacts().is_empty());
    }

    #[test]
    fn impl_round_one_can_go_back_to_brainstorm_on_skip_path() {
        assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::BrainstormRunning));
        // Still allowed to rewind into sharding for the normal path.
        assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::ShardingRunning));
        // Later rounds must not jump straight to brainstorm.
        assert!(!Phase::ImplementationRound(2).can_transition_to(&Phase::BrainstormRunning));
    }

    #[test]
    fn final_validation_metadata() {
        assert_eq!(Phase::FinalValidation(2).label(), "Final Validation");
        assert_eq!(
            format!("{}", Phase::FinalValidation(2)),
            "Final Validation Round 2"
        );
        assert_eq!(
            Phase::FinalValidation(1).description(),
            "AI agent verifying the original goal against the live workspace"
        );
        assert_eq!(
            Phase::FinalValidation(1).required_artifacts(),
            vec![crate::artifacts::ArtifactKind::Spec]
        );
    }

    #[test]
    fn final_validation_no_longer_has_direct_edges_from_review_or_impl() {
        // The pre-existing direct edges are removed; normal pipeline entries
        // into FinalValidation must go through Simplification.
        assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::FinalValidation(2)));
        assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::FinalValidation(3)));
        assert!(!Phase::ImplementationRound(1).can_transition_to(&Phase::FinalValidation(1)));
        assert!(!Phase::ImplementationRound(2).can_transition_to(&Phase::FinalValidation(2)));
    }

    #[test]
    fn final_validation_outbound_transitions() {
        assert!(Phase::FinalValidation(1).can_transition_to(&Phase::Done));
        assert!(Phase::FinalValidation(1).can_transition_to(&Phase::ImplementationRound(2)));
        assert!(Phase::FinalValidation(3).can_transition_to(&Phase::ImplementationRound(4)));
        // r+2 not allowed; only r+1.
        assert!(!Phase::FinalValidation(1).can_transition_to(&Phase::ImplementationRound(3)));
        assert!(Phase::FinalValidation(1).can_transition_to(&Phase::BlockedNeedsUser));
    }

    #[test]
    fn blocked_needs_user_to_done_is_statically_allowed() {
        // The static graph permits the edge so the operator-facing affordance
        // can be surfaced; the runtime guard in `execute_transition` enforces
        // `block_origin == FinalValidation`.
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::Done));
    }

    #[test]
    fn final_validation_rewind_transitions() {
        assert!(Phase::FinalValidation(2).can_transition_to(&Phase::ReviewRound(2)));
        // Cross-round rewind rejected.
        assert!(!Phase::FinalValidation(2).can_transition_to(&Phase::ReviewRound(1)));
        // Skip-to-impl rewind: only valid at round 1.
        assert!(Phase::FinalValidation(1).can_transition_to(&Phase::ImplementationRound(1)));
        assert!(!Phase::FinalValidation(2).can_transition_to(&Phase::ImplementationRound(2)));
    }

    #[test]
    fn simplification_metadata() {
        assert_eq!(Phase::Simplification(2).label(), "Simplification");
        assert_eq!(
            format!("{}", Phase::Simplification(2)),
            "Simplification Round 2"
        );
        assert_eq!(
            Phase::Simplification(1).description(),
            "AI agent applying behavior-preserving simplifications before final validation"
        );
        assert_eq!(
            Phase::Simplification(1).required_artifacts(),
            vec![crate::artifacts::ArtifactKind::Spec]
        );
    }

    #[test]
    fn simplification_inbound_transitions() {
        // Normal converging-loop entry.
        assert!(Phase::ReviewRound(2).can_transition_to(&Phase::Simplification(2)));
        // Cross-round combinations are rejected.
        assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::Simplification(3)));
        // Skip-to-impl entry — only at round 1.
        assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::Simplification(1)));
        assert!(!Phase::ImplementationRound(2).can_transition_to(&Phase::Simplification(2)));
        // Operator can re-enter simplification from a block.
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::Simplification(3)));
    }

    #[test]
    fn simplification_outbound_transitions() {
        // Happy path into final validation; matched round only.
        assert!(Phase::Simplification(2).can_transition_to(&Phase::FinalValidation(2)));
        assert!(!Phase::Simplification(2).can_transition_to(&Phase::FinalValidation(1)));
        assert!(!Phase::Simplification(2).can_transition_to(&Phase::FinalValidation(3)));
        // Failure path.
        assert!(Phase::Simplification(2).can_transition_to(&Phase::BlockedNeedsUser));
        // Direct shortcut to Done is rejected.
        assert!(!Phase::Simplification(2).can_transition_to(&Phase::Done));
    }

    #[test]
    fn simplification_rewind_transitions() {
        assert!(Phase::Simplification(2).can_transition_to(&Phase::ReviewRound(2)));
        // Cross-round rewind rejected.
        assert!(!Phase::Simplification(2).can_transition_to(&Phase::ReviewRound(1)));
        // Skip-to-impl rewind only at round 1.
        assert!(Phase::Simplification(1).can_transition_to(&Phase::ImplementationRound(1)));
        assert!(!Phase::Simplification(2).can_transition_to(&Phase::ImplementationRound(2)));
    }

    #[test]
    fn final_validation_inbound_edges_only_simplification_or_block() {
        // The blocked recovery exception is preserved.
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::FinalValidation(1)));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::FinalValidation(7)));
        // Sweep every other Phase variant: only Simplification(r) can reach
        // FinalValidation(r), and BlockedNeedsUser can reach any round.
        let candidates = [
            Phase::IdeaInput,
            Phase::BrainstormRunning,
            Phase::SpecReviewRunning,
            Phase::SpecReviewPaused,
            Phase::PlanningRunning,
            Phase::PlanReviewRunning,
            Phase::PlanReviewPaused,
            Phase::ShardingRunning,
            Phase::SkipToImplPending,
            Phase::ImplementationRound(2),
            Phase::ReviewRound(2),
            Phase::BuilderRecovery(2),
            Phase::BuilderRecoveryPlanReview(2),
            Phase::BuilderRecoverySharding(2),
            Phase::GitGuardPending,
            Phase::FinalValidation(2),
            Phase::Done,
        ];
        for from in candidates {
            assert!(
                !from.can_transition_to(&Phase::FinalValidation(2)),
                "unexpected inbound edge into FinalValidation from {:?}",
                from
            );
        }
        // Round-1 skip-to-impl direct edge is also gone.
        assert!(!Phase::ImplementationRound(1).can_transition_to(&Phase::FinalValidation(1)));
    }
}
