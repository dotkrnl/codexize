use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    IdeaInput,
    BrainstormRunning,
    SpecReviewRunning,
    SpecReviewPaused,
    PlanningRunning,
    PlanReviewRunning,
    PlanReviewPaused,
    ShardingRunning,
    SkipToImplPending, // New phase
    /// Coder agent is working on the current task in round N.
    ImplementationRound(u32),
    /// Reviewer agent is checking the current task's work in round N.
    ReviewRound(u32),
    /// Builder-only recovery stage that repairs artifacts and reconciles queue state.
    ///
    /// The stored round is the builder round that triggered recovery; successful recovery
    /// resumes into the next implementation round (round + 1).
    BuilderRecovery(u32),
    Done,
    BlockedNeedsUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ArtifactKind {
    Spec,
    SpecReview,
    Plan,
    PlanReview,
    CodeReview,
    Implementation,
}

impl Phase {
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
            Phase::ImplementationRound(r) => format!("Builder: coder r{r}"),
            Phase::ReviewRound(r) => format!("Builder: reviewer r{r}"),
            Phase::BuilderRecovery(_) => "Builder Recovery".to_string(),
            Phase::Done => "Done".to_string(),
            Phase::BlockedNeedsUser => "Blocked".to_string(),
            Phase::SkipToImplPending => "Skip Confirmation".to_string(),
        }
    }

    /// Returns true if a transition from `self` to `target` is valid.
    pub fn can_transition_to(&self, target: &Phase) -> bool {
        use Phase::*;
        match (self, target) {
            // Forward transitions
            (IdeaInput, BrainstormRunning) => true,
            (BrainstormRunning, SpecReviewRunning) => true,
            (BrainstormRunning, BlockedNeedsUser) => true,
            (BrainstormRunning, SkipToImplPending) => true, // New transition
            (SpecReviewRunning, SpecReviewPaused) => true,
            (SpecReviewRunning, PlanningRunning) => true,
            (SpecReviewRunning, BlockedNeedsUser) => true,
            (SpecReviewPaused, SpecReviewRunning) => true,
            (SpecReviewPaused, PlanningRunning) => true,
            (SpecReviewPaused, BlockedNeedsUser) => true,
            (SkipToImplPending, ImplementationRound(_)) => true, // New transition
            (SkipToImplPending, SpecReviewRunning) => true, // New transition
            (SkipToImplPending, BlockedNeedsUser) => true, // New transition
            // New forward transitions for Plan Review
            (PlanningRunning, PlanReviewRunning) => true,
            (PlanReviewRunning, ShardingRunning) => true,
            (PlanReviewRunning, BlockedNeedsUser) => true,
            (PlanReviewPaused, PlanReviewRunning) => true,
            (PlanReviewPaused, ShardingRunning) => true,
            (PlanReviewPaused, BlockedNeedsUser) => true,
            (BlockedNeedsUser, PlanReviewRunning) => true,
            (PlanningRunning, BlockedNeedsUser) => true,
            (ShardingRunning, ImplementationRound(1)) => true,
            (ShardingRunning, BlockedNeedsUser) => true,
            (ImplementationRound(r), ReviewRound(r2)) if *r == *r2 => true,
            (ImplementationRound(r), BuilderRecovery(r2)) if *r == *r2 => true,
            (ImplementationRound(_), BlockedNeedsUser) => true,
            (ReviewRound(r), ImplementationRound(r2)) if *r2 == *r + 1 => true,
            (ReviewRound(_), Done) => true,
            (ReviewRound(_), BlockedNeedsUser) => true,
            (ReviewRound(r), BuilderRecovery(r2)) if *r == *r2 => true,
            (BuilderRecovery(r), ImplementationRound(r2)) if *r2 == *r + 1 => true,
            (BuilderRecovery(_), BlockedNeedsUser) => true,
            (BlockedNeedsUser, BrainstormRunning) => true,
            (BlockedNeedsUser, SpecReviewRunning) => true,
            (BlockedNeedsUser, PlanningRunning) => true,
            (BlockedNeedsUser, ShardingRunning) => true,
            (BlockedNeedsUser, ImplementationRound(_)) => true,
            (BlockedNeedsUser, ReviewRound(_)) => true,
            (BlockedNeedsUser, BuilderRecovery(_)) => true,
            // Backward transitions (go_back)
            (BrainstormRunning, IdeaInput) => true,
            (SpecReviewRunning, BrainstormRunning) => true,
            (SpecReviewPaused, BrainstormRunning) => true,
            (PlanningRunning, SpecReviewRunning) => true,
            (ShardingRunning, PlanReviewRunning) => true, // Changed from PlanningRunning
            // New backward transitions for Plan Review
            (PlanReviewRunning, PlanningRunning) => true,
            (PlanReviewRunning, PlanReviewPaused) => true,
            (PlanReviewPaused, PlanningRunning) => true,
            (ImplementationRound(r), ShardingRunning) if *r <= 1 => true,
            (ImplementationRound(r), ReviewRound(r2)) if *r2 == *r - 1 => true,
            (ReviewRound(r), ImplementationRound(r2)) if *r == *r2 => true,
            _ => false,
        }
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
            Phase::ImplementationRound(_) => {
                vec![ArtifactKind::Plan, ArtifactKind::Implementation]
            }
            Phase::ReviewRound(_) => vec![ArtifactKind::CodeReview],
            Phase::BuilderRecovery(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::SkipToImplPending => vec![], // No artifacts required for this phase itself
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
            Phase::Done => "Run completed successfully",
            Phase::BlockedNeedsUser => "Blocked - requires user intervention",
            Phase::SkipToImplPending => "Awaiting user confirmation to skip to implementation",
        }
    }

    /// Human-readable display name, including round numbers for parameterized phases.
    pub fn display_name(&self) -> String {
        match self {
            Phase::ImplementationRound(n) => format!("Implementation Round {n}"),
            Phase::ReviewRound(n) => format!("Review Round {n}"),
            Phase::BuilderRecovery(_) => "Builder Recovery".to_string(),
            _ => self.label(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Phase;

    #[test]
    fn plan_review_forward_transitions() {
        assert!(Phase::PlanningRunning.can_transition_to(&Phase::PlanReviewRunning));
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
        assert_eq!(Phase::PlanReviewRunning.display_name(), "Plan Review");
        assert_eq!(Phase::PlanReviewPaused.display_name(), "Plan Review");
    }

    #[test]
    fn builder_recovery_transitions() {
        assert!(Phase::ImplementationRound(3).can_transition_to(&Phase::BuilderRecovery(3)));
        assert!(Phase::ReviewRound(3).can_transition_to(&Phase::BuilderRecovery(3)));
        assert!(Phase::BuilderRecovery(3).can_transition_to(&Phase::ImplementationRound(4)));
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
        assert!(!Phase::SkipToImplPending.can_transition_to(&Phase::BrainstormRunning));
        assert!(!Phase::SkipToImplPending.can_transition_to(&Phase::PlanningRunning));
    }
}
