use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    IdeaInput,
    BrainstormRunning,
    SpecReviewRunning,
    SpecReviewPaused,
    PlanningRunning,
    ShardingRunning,
    /// Coder agent is working on the current task in round N.
    ImplementationRound(u32),
    /// Reviewer agent is checking the current task's work in round N.
    ReviewRound(u32),
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
            Phase::ShardingRunning => "Sharding".to_string(),
            Phase::ImplementationRound(r) => format!("Builder: coder r{r}"),
            Phase::ReviewRound(r) => format!("Builder: reviewer r{r}"),
            Phase::Done => "Done".to_string(),
            Phase::BlockedNeedsUser => "Blocked".to_string(),
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
            (SpecReviewRunning, SpecReviewPaused) => true,
            (SpecReviewRunning, PlanningRunning) => true,
            (SpecReviewRunning, BlockedNeedsUser) => true,
            (SpecReviewPaused, SpecReviewRunning) => true,
            (SpecReviewPaused, PlanningRunning) => true,
            (SpecReviewPaused, BlockedNeedsUser) => true,
            (PlanningRunning, ShardingRunning) => true,
            (PlanningRunning, BlockedNeedsUser) => true,
            (ShardingRunning, ImplementationRound(1)) => true,
            (ShardingRunning, BlockedNeedsUser) => true,
            (ImplementationRound(r), ReviewRound(r2)) if *r == *r2 => true,
            (ReviewRound(r), ImplementationRound(r2)) if *r2 == *r + 1 => true,
            (ReviewRound(_), Done) => true,
            (ReviewRound(_), BlockedNeedsUser) => true,
            (BlockedNeedsUser, BrainstormRunning) => true,
            (BlockedNeedsUser, SpecReviewRunning) => true,
            (BlockedNeedsUser, PlanningRunning) => true,
            (BlockedNeedsUser, ShardingRunning) => true,
            (BlockedNeedsUser, ImplementationRound(_)) => true,
            (BlockedNeedsUser, ReviewRound(_)) => true,
            // Backward transitions (go_back)
            (BrainstormRunning, IdeaInput) => true,
            (SpecReviewRunning, BrainstormRunning) => true,
            (SpecReviewPaused, BrainstormRunning) => true,
            (PlanningRunning, SpecReviewRunning) => true,
            (ShardingRunning, PlanningRunning) => true,
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
            Phase::ShardingRunning => vec![ArtifactKind::Plan],
            Phase::ImplementationRound(_) => {
                vec![ArtifactKind::Plan, ArtifactKind::Implementation]
            }
            Phase::ReviewRound(_) => vec![ArtifactKind::CodeReview],
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
            Phase::ShardingRunning => "Splitting plan into actionable tasks",
            Phase::ImplementationRound(_) => "AI agent implementing code",
            Phase::ReviewRound(_) => "AI agent reviewing implementation",
            Phase::Done => "Run completed successfully",
            Phase::BlockedNeedsUser => "Blocked - requires user intervention",
        }
    }

    /// Human-readable display name, including round numbers for parameterized phases.
    pub fn display_name(&self) -> String {
        match self {
            Phase::ImplementationRound(n) => format!("Implementation Round {n}"),
            Phase::ReviewRound(n) => format!("Review Round {n}"),
            _ => self.label(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forward_transitions() {
        assert!(Phase::IdeaInput.can_transition_to(&Phase::BrainstormRunning));
        assert!(Phase::BrainstormRunning.can_transition_to(&Phase::SpecReviewRunning));
        assert!(Phase::SpecReviewRunning.can_transition_to(&Phase::PlanningRunning));
        assert!(Phase::PlanningRunning.can_transition_to(&Phase::ShardingRunning));
        assert!(Phase::ShardingRunning.can_transition_to(&Phase::ImplementationRound(1)));
        assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::ReviewRound(1)));
        assert!(Phase::ReviewRound(1).can_transition_to(&Phase::ImplementationRound(2)));
        assert!(Phase::ReviewRound(2).can_transition_to(&Phase::Done));
    }

    #[test]
    fn test_backward_transitions() {
        assert!(Phase::BrainstormRunning.can_transition_to(&Phase::IdeaInput));
        assert!(Phase::SpecReviewRunning.can_transition_to(&Phase::BrainstormRunning));
        assert!(Phase::PlanningRunning.can_transition_to(&Phase::SpecReviewRunning));
        assert!(Phase::ShardingRunning.can_transition_to(&Phase::PlanningRunning));
        assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::ShardingRunning));
    }

    #[test]
    fn test_blocked_recovery_transitions() {
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::BrainstormRunning));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::SpecReviewRunning));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::PlanningRunning));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::ShardingRunning));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::ImplementationRound(3)));
        assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::ReviewRound(3)));
    }

    #[test]
    fn test_invalid_transitions_rejected() {
        assert!(!Phase::IdeaInput.can_transition_to(&Phase::Done));
        assert!(!Phase::IdeaInput.can_transition_to(&Phase::PlanningRunning));
        assert!(!Phase::BrainstormRunning.can_transition_to(&Phase::Done));
        // Impl round can only go to same or previous review round
        assert!(!Phase::ImplementationRound(3).can_transition_to(&Phase::ReviewRound(1)));
        assert!(!Phase::ImplementationRound(3).can_transition_to(&Phase::ReviewRound(5)));
        // Review round can only go to next or same impl round
        assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::ImplementationRound(4)));
        assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::ImplementationRound(1)));
    }

    #[test]
    fn test_parameterized_round_transitions() {
        // Forward: impl -> review same round
        assert!(Phase::ImplementationRound(5).can_transition_to(&Phase::ReviewRound(5)));
        // Backward: impl -> review previous round
        assert!(Phase::ImplementationRound(5).can_transition_to(&Phase::ReviewRound(4)));
        // Other impl->review transitions are not allowed
        assert!(!Phase::ImplementationRound(5).can_transition_to(&Phase::ReviewRound(3)));
        assert!(!Phase::ImplementationRound(5).can_transition_to(&Phase::ReviewRound(6)));

        // Forward: review -> impl next round
        assert!(Phase::ReviewRound(3).can_transition_to(&Phase::ImplementationRound(4)));
        // Backward: review -> impl same round
        assert!(Phase::ReviewRound(3).can_transition_to(&Phase::ImplementationRound(3)));
        // Other review->impl transitions are not allowed
        assert!(!Phase::ReviewRound(3).can_transition_to(&Phase::ImplementationRound(2)));
        assert!(!Phase::ReviewRound(3).can_transition_to(&Phase::ImplementationRound(5)));
    }

    #[test]
    fn test_required_artifacts() {
        assert!(Phase::IdeaInput.required_artifacts().is_empty());
        assert_eq!(Phase::SpecReviewRunning.required_artifacts(), vec![ArtifactKind::Spec]);
        assert_eq!(
            Phase::PlanningRunning.required_artifacts(),
            vec![ArtifactKind::Spec, ArtifactKind::Plan]
        );
        assert_eq!(Phase::ShardingRunning.required_artifacts(), vec![ArtifactKind::Plan]);
    }

    #[test]
    fn test_display_name() {
        assert_eq!(Phase::IdeaInput.display_name(), "Idea Input");
        assert_eq!(Phase::ImplementationRound(3).display_name(), "Implementation Round 3");
        assert_eq!(Phase::ReviewRound(2).display_name(), "Review Round 2");
    }
}
