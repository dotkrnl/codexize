//! Modal kinds and stage identifiers.
use serde::Serialize;

/// Operator-visible stage-error target used by stage-scoped modals and
/// retry commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub enum StageId {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    RepoStateUpdate,
    Sharding,
    Implementation,
    Recovery,
    RecoveryPlanReview,
    RecoverySharding,
    Review,
    Simplification,
    FinalValidation,
    Dreaming,
}

/// Modal kinds the runtime asks the UI to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModalKind {
    SkipToImpl,
    GitGuard,
    QuitRunningAgent,
    CancelSession,
    InteractiveExitPrompt,
    SpecReviewPaused,
    PlanReviewPaused,
    StageError(StageId),
    FinalValidationBlocked,
    DreamingDecision,
}
