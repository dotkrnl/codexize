use crate::artifacts::ArtifactKind;
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
    /// resumes into `BuilderRecoveryPlanReview` of the same round.
    BuilderRecovery(u32),
    /// Non-interactive plan review inserted after a builder recovery stage completes.
    /// Verifies the recovered spec/plan is coherent before sharding.
    BuilderRecoveryPlanReview(u32),
    /// Recovery-mode re-sharding inserted after a successful recovery plan review.
    /// Regenerates the task queue from the recovered spec/plan.
    BuilderRecoverySharding(u32),
    /// Interactive non-coder run advanced HEAD under `GuardMode::AskOperator`;
    /// the modal is up and the operator must choose reset or keep before the
    /// run can finalize.
    GitGuardPending,
    /// Final goal validation runs once per queue-empty pre-`Done` boundary.
    /// The stored round is the coder round whose work is being validated.
    FinalValidation(u32),
    Done,
    BlockedNeedsUser,
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
            (SkipToImplPending, SpecReviewRunning) => true,      // New transition
            (SkipToImplPending, BlockedNeedsUser) => true,       // New transition
            (SkipToImplPending, Done) => true,                   // nothing-to-do outcome
            (SkipToImplPending, BrainstormRunning) => true,      // decline nothing-to-do → retry
            // New forward transitions for Plan Review
            (PlanningRunning, PlanReviewRunning) => true,
            (PlanningRunning, ShardingRunning) => true,
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
            (BuilderRecovery(r), BuilderRecoveryPlanReview(r2)) if *r == *r2 => true,
            (BuilderRecovery(r), BuilderRecoverySharding(r2)) if *r == *r2 => true,
            (BuilderRecovery(_), BlockedNeedsUser) => true,
            (BuilderRecoveryPlanReview(r), BuilderRecoverySharding(r2)) if *r == *r2 => true,
            (BuilderRecoveryPlanReview(r), BuilderRecovery(r2)) if *r == *r2 => true,
            (BuilderRecoveryPlanReview(_), BlockedNeedsUser) => true,
            (BuilderRecoverySharding(r), ImplementationRound(r2)) if *r2 == *r + 1 => true,
            (BuilderRecoverySharding(_), BlockedNeedsUser) => true,
            (BlockedNeedsUser, BrainstormRunning) => true,
            (BlockedNeedsUser, SpecReviewRunning) => true,
            (BlockedNeedsUser, PlanningRunning) => true,
            (BlockedNeedsUser, ShardingRunning) => true,
            (BlockedNeedsUser, ImplementationRound(_)) => true,
            (BlockedNeedsUser, ReviewRound(_)) => true,
            (BlockedNeedsUser, BuilderRecovery(_)) => true,
            // Git guard pending: inbound from every non-coder running phase
            // that may launch under AskOperator. Outbound covers both the
            // reset-failure successors and every keep-success successor a
            // brainstorm / planning / interactive recovery run could reach.
            (BrainstormRunning, GitGuardPending) => true,
            (PlanningRunning, GitGuardPending) => true,
            (BuilderRecovery(_), GitGuardPending) => true,
            (GitGuardPending, BlockedNeedsUser) => true,
            (GitGuardPending, Done) => true,
            (GitGuardPending, SpecReviewRunning) => true,
            (GitGuardPending, SkipToImplPending) => true,
            (GitGuardPending, PlanReviewRunning) => true,
            (GitGuardPending, BuilderRecoveryPlanReview(_)) => true,
            // Final validation: queue-empty/pre-`Done` boundary on every
            // code-producing path. Round identifies the coder round whose
            // work is being validated.
            (ReviewRound(r), FinalValidation(r2)) if *r == *r2 => true,
            (ImplementationRound(1), FinalValidation(1)) => true,
            (FinalValidation(_), Done) => true,
            (FinalValidation(r), ImplementationRound(r2)) if *r2 == *r + 1 => true,
            (FinalValidation(_), BlockedNeedsUser) => true,
            // Force-ship from a final-validation block. The runtime guard in
            // `execute_transition` rejects this transition when the block did
            // not originate from final validation.
            (BlockedNeedsUser, Done) => true,
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
            // Skip-to-implementation sessions rewind past sharding back to brainstorm,
            // since sharding never ran on this path.
            (ImplementationRound(r), BrainstormRunning) if *r <= 1 => true,
            (ImplementationRound(r), ReviewRound(r2)) if *r2 == *r - 1 => true,
            (ReviewRound(r), ImplementationRound(r2)) if *r == *r2 => true,
            // Rewind transitions out of final validation.
            (FinalValidation(r), ReviewRound(r2)) if *r == *r2 && *r >= 1 => true,
            (FinalValidation(1), ImplementationRound(1)) => true,
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
            Phase::ImplementationRound(_) => vec![ArtifactKind::Plan, ArtifactKind::Tasks],
            Phase::ReviewRound(_) => vec![ArtifactKind::CodeReview],
            Phase::BuilderRecovery(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::BuilderRecoveryPlanReview(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::BuilderRecoverySharding(_) => vec![ArtifactKind::Spec, ArtifactKind::Plan],
            Phase::SkipToImplPending => vec![], // No artifacts required for this phase itself
            Phase::GitGuardPending => vec![],
            Phase::FinalValidation(_) => vec![ArtifactKind::Spec],
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
        }
    }

    /// Human-readable display name, including round numbers for parameterized phases.
    pub fn display_name(&self) -> String {
        match self {
            Phase::ImplementationRound(n) => format!("Implementation Round {n}"),
            Phase::ReviewRound(n) => format!("Review Round {n}"),
            Phase::BuilderRecovery(_) => "Builder Recovery".to_string(),
            Phase::BuilderRecoveryPlanReview(_) => "Recovery Plan Review".to_string(),
            Phase::BuilderRecoverySharding(_) => "Recovery Sharding".to_string(),
            Phase::FinalValidation(n) => format!("Final Validation Round {n}"),
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
        assert_eq!(Phase::PlanReviewRunning.display_name(), "Plan Review");
        assert_eq!(Phase::PlanReviewPaused.display_name(), "Plan Review");
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
            Phase::FinalValidation(2).display_name(),
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
    fn final_validation_forward_transitions_from_review_and_skip_to_impl() {
        // ReviewRound -> FinalValidation matches by round.
        assert!(Phase::ReviewRound(2).can_transition_to(&Phase::FinalValidation(2)));
        // Cross-round combinations are rejected.
        assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::FinalValidation(3)));
        // Skip-to-impl exit only valid for round 1 (single-commit promise).
        assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::FinalValidation(1)));
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

}
