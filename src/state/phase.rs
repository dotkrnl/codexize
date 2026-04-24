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
