use super::{Phase, PhaseKind};
use PhaseKind as P;
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
    TransitionEdge::new(P::PlanReviewRunning, P::WaitingToImplement),
    TransitionEdge::new(P::PlanReviewRunning, P::ShardingRunning),
    TransitionEdge::new(P::PlanReviewRunning, P::BlockedNeedsUser),
    TransitionEdge::new(P::PlanReviewPaused, P::PlanReviewRunning),
    TransitionEdge::new(P::PlanReviewPaused, P::WaitingToImplement),
    TransitionEdge::new(P::PlanReviewPaused, P::ShardingRunning),
    TransitionEdge::new(P::PlanReviewPaused, P::BlockedNeedsUser),
    TransitionEdge::new(P::PlanReviewPaused, P::Cancelled),
    TransitionEdge::new(P::BlockedNeedsUser, P::PlanReviewRunning),
    TransitionEdge::new(P::WaitingToImplement, P::RepoStateUpdateRunning),
    TransitionEdge::new(P::WaitingToImplement, P::ShardingRunning),
    TransitionEdge::new(P::WaitingToImplement, P::Cancelled),
    TransitionEdge::new(P::RepoStateUpdateRunning, P::ShardingRunning),
    TransitionEdge::new(P::RepoStateUpdateRunning, P::BlockedNeedsUser),
    TransitionEdge::new(P::RepoStateUpdateRunning, P::Cancelled),
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
    TransitionEdge::new(P::FinalValidation, P::DreamingPending),
    TransitionEdge::new(P::DreamingPending, P::Done),
    TransitionEdge::new(P::DreamingPending, P::Dreaming),
    TransitionEdge::new(P::Dreaming, P::Done),
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
    TransitionEdge::new(P::IdeaInput, P::Cancelled),
    TransitionEdge::new(P::BrainstormRunning, P::Cancelled),
    TransitionEdge::new(P::SpecReviewRunning, P::Cancelled),
    TransitionEdge::new(P::SpecReviewPaused, P::Cancelled),
    TransitionEdge::new(P::SkipToImplPending, P::Cancelled),
    TransitionEdge::new(P::PlanningRunning, P::Cancelled),
    TransitionEdge::new(P::PlanReviewRunning, P::Cancelled),
    TransitionEdge::new(P::BlockedNeedsUser, P::Cancelled),
    TransitionEdge::new(P::GitGuardPending, P::Cancelled),
    TransitionEdge::new(P::ShardingRunning, P::Cancelled),
    TransitionEdge::new(P::ImplementationRound, P::Cancelled),
    TransitionEdge::new(P::ReviewRound, P::Cancelled),
    TransitionEdge::new(P::BuilderRecovery, P::Cancelled),
    TransitionEdge::new(P::BuilderRecoveryPlanReview, P::Cancelled),
    TransitionEdge::new(P::BuilderRecoverySharding, P::Cancelled),
    TransitionEdge::new(P::FinalValidation, P::Cancelled),
    TransitionEdge::new(P::DreamingPending, P::Cancelled),
    TransitionEdge::new(P::Dreaming, P::Cancelled),
    TransitionEdge::new(P::Simplification, P::Cancelled),
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
pub(super) fn can_transition(from: &Phase, to: &Phase) -> bool {
    TRANSITION_EDGES.iter().any(|edge| edge.allows(from, to))
}
