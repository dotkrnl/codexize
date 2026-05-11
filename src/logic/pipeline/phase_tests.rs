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
}

#[test]
fn skip_to_impl_pending_transitions() {
    assert!(Phase::BrainstormRunning.can_transition_to(&Phase::SkipToImplPending));
    assert!(Phase::SkipToImplPending.can_transition_to(&Phase::ImplementationRound(1)));
    assert!(Phase::SkipToImplPending.can_transition_to(&Phase::SpecReviewRunning));
    assert!(Phase::SkipToImplPending.can_transition_to(&Phase::BlockedNeedsUser));

    assert!(!Phase::SpecReviewRunning.can_transition_to(&Phase::SkipToImplPending));
    assert!(!Phase::PlanningRunning.can_transition_to(&Phase::SkipToImplPending));
    assert!(!Phase::SkipToImplPending.can_transition_to(&Phase::PlanningRunning));
}

#[test]
fn git_guard_pending_edges() {
    assert!(Phase::BrainstormRunning.can_transition_to(&Phase::GitGuardPending));
    assert!(Phase::PlanningRunning.can_transition_to(&Phase::GitGuardPending));
    assert!(Phase::BuilderRecovery(2).can_transition_to(&Phase::GitGuardPending));
    assert!(Phase::GitGuardPending.can_transition_to(&Phase::BlockedNeedsUser));
    assert!(Phase::GitGuardPending.can_transition_to(&Phase::Done));
    assert!(Phase::GitGuardPending.can_transition_to(&Phase::SpecReviewRunning));
    assert!(Phase::GitGuardPending.can_transition_to(&Phase::SkipToImplPending));
    assert!(Phase::GitGuardPending.can_transition_to(&Phase::PlanReviewRunning));
    assert!(Phase::GitGuardPending.can_transition_to(&Phase::BuilderRecoveryPlanReview(3)));

    assert!(!Phase::SpecReviewRunning.can_transition_to(&Phase::GitGuardPending));
    assert!(!Phase::ImplementationRound(1).can_transition_to(&Phase::GitGuardPending));
    assert!(!Phase::GitGuardPending.can_transition_to(&Phase::ImplementationRound(1)));
    assert!(!Phase::GitGuardPending.can_transition_to(&Phase::ShardingRunning));
    assert!(!Phase::GitGuardPending.can_transition_to(&Phase::GitGuardPending));
}

#[test]
fn impl_round_one_can_go_back_to_brainstorm_on_skip_path() {
    assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::BrainstormRunning));
    assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::ShardingRunning));
    assert!(!Phase::ImplementationRound(2).can_transition_to(&Phase::BrainstormRunning));
}

#[test]
fn final_validation_edges() {
    assert_eq!(Phase::FinalValidation(2).label(), "Final Validation");
    assert_eq!(
        format!("{}", Phase::FinalValidation(2)),
        "Final Validation Round 2"
    );
    assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::FinalValidation(2)));
    assert!(!Phase::ImplementationRound(1).can_transition_to(&Phase::FinalValidation(1)));
    assert!(Phase::FinalValidation(1).can_transition_to(&Phase::Done));
    assert!(Phase::FinalValidation(1).can_transition_to(&Phase::ImplementationRound(2)));
    assert!(!Phase::FinalValidation(1).can_transition_to(&Phase::ImplementationRound(3)));
    assert!(Phase::FinalValidation(1).can_transition_to(&Phase::BlockedNeedsUser));
    assert!(Phase::FinalValidation(1).can_transition_to(&Phase::DreamingPending));
    assert!(Phase::DreamingPending.can_transition_to(&Phase::Done));
    assert!(Phase::DreamingPending.can_transition_to(&Phase::Dreaming(1)));
    assert!(Phase::Dreaming(1).can_transition_to(&Phase::Done));
    assert!(!Phase::DreamingPending.can_transition_to(&Phase::FinalValidation(1)));
    assert!(Phase::FinalValidation(2).can_transition_to(&Phase::ReviewRound(2)));
    assert!(!Phase::FinalValidation(2).can_transition_to(&Phase::ReviewRound(1)));
    assert!(Phase::FinalValidation(1).can_transition_to(&Phase::ImplementationRound(1)));
    assert!(!Phase::FinalValidation(2).can_transition_to(&Phase::ImplementationRound(2)));
    assert_eq!(Phase::DreamingPending.label(), "Dreaming");
    assert_eq!(Phase::Dreaming(2).label(), "Dreaming");
}

#[test]
fn blocked_needs_user_to_done_is_statically_allowed() {
    assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::Done));
}

#[test]
fn simplification_edges() {
    assert_eq!(Phase::Simplification(2).label(), "Simplification");
    assert_eq!(
        format!("{}", Phase::Simplification(2)),
        "Simplification Round 2"
    );
    assert!(Phase::ReviewRound(2).can_transition_to(&Phase::Simplification(2)));
    assert!(!Phase::ReviewRound(2).can_transition_to(&Phase::Simplification(3)));
    assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::Simplification(1)));
    assert!(!Phase::ImplementationRound(2).can_transition_to(&Phase::Simplification(2)));
    assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::Simplification(3)));
    assert!(Phase::Simplification(2).can_transition_to(&Phase::FinalValidation(2)));
    assert!(!Phase::Simplification(2).can_transition_to(&Phase::FinalValidation(3)));
    assert!(Phase::Simplification(2).can_transition_to(&Phase::BlockedNeedsUser));
    assert!(!Phase::Simplification(2).can_transition_to(&Phase::Done));
    assert!(Phase::Simplification(2).can_transition_to(&Phase::ReviewRound(2)));
    assert!(!Phase::Simplification(2).can_transition_to(&Phase::ReviewRound(1)));
    assert!(Phase::Simplification(1).can_transition_to(&Phase::ImplementationRound(1)));
    assert!(!Phase::Simplification(2).can_transition_to(&Phase::ImplementationRound(2)));
}

#[test]
fn final_validation_inbound_edges_only_simplification_or_block() {
    assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::FinalValidation(1)));
    assert!(Phase::BlockedNeedsUser.can_transition_to(&Phase::FinalValidation(7)));

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
    assert!(!Phase::ImplementationRound(1).can_transition_to(&Phase::FinalValidation(1)));
}

#[test]
fn waiting_to_implement_transitions() {
    assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::WaitingToImplement));
    assert!(Phase::PlanReviewPaused.can_transition_to(&Phase::WaitingToImplement));
    assert!(Phase::WaitingToImplement.can_transition_to(&Phase::ShardingRunning));
    assert!(Phase::WaitingToImplement.can_transition_to(&Phase::RepoStateUpdateRunning));
    assert!(Phase::RepoStateUpdateRunning.can_transition_to(&Phase::ShardingRunning));
    assert!(Phase::WaitingToImplement.can_transition_to(&Phase::Cancelled));
    assert!(Phase::RepoStateUpdateRunning.can_transition_to(&Phase::Cancelled));
    assert!(!Phase::WaitingToImplement.can_transition_to(&Phase::WaitingToImplement));
    assert!(!Phase::RepoStateUpdateRunning.can_transition_to(&Phase::RepoStateUpdateRunning));
}

#[test]
fn cancelled_is_terminal() {
    assert!(!Phase::Cancelled.can_transition_to(&Phase::Done));
    assert!(!Phase::Cancelled.can_transition_to(&Phase::BrainstormRunning));
    assert!(!Phase::Cancelled.can_transition_to(&Phase::WaitingToImplement));
    assert!(!Phase::Cancelled.can_transition_to(&Phase::RepoStateUpdateRunning));
    assert!(!Phase::Cancelled.can_transition_to(&Phase::Cancelled));
}

#[test]
fn new_phase_labels() {
    assert_eq!(Phase::WaitingToImplement.label(), "Waiting to implement");
    assert_eq!(Phase::RepoStateUpdateRunning.label(), "Updating plan");
    assert_eq!(Phase::Cancelled.label(), "Cancelled");
    assert_eq!(
        format!("{}", Phase::WaitingToImplement),
        "Waiting to implement"
    );
    assert_eq!(
        format!("{}", Phase::RepoStateUpdateRunning),
        "Updating plan"
    );
    assert_eq!(format!("{}", Phase::Cancelled), "Cancelled");
}

#[test]
fn running_phases_can_transition_to_cancelled() {
    assert!(Phase::BrainstormRunning.can_transition_to(&Phase::Cancelled));
    assert!(Phase::SpecReviewRunning.can_transition_to(&Phase::Cancelled));
    assert!(Phase::PlanningRunning.can_transition_to(&Phase::Cancelled));
    assert!(Phase::PlanReviewRunning.can_transition_to(&Phase::Cancelled));
    assert!(Phase::ShardingRunning.can_transition_to(&Phase::Cancelled));
    assert!(Phase::ImplementationRound(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::ReviewRound(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::BuilderRecovery(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::BuilderRecoveryPlanReview(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::BuilderRecoverySharding(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::FinalValidation(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::Dreaming(1).can_transition_to(&Phase::Cancelled));
    assert!(Phase::Simplification(1).can_transition_to(&Phase::Cancelled));
}
