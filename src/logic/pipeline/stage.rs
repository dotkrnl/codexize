use serde::{Deserialize, Serialize};
#[path = "stage_graph.rs"]
mod stage_graph;
// `EnumString` is intentionally not derived: no caller parses stage names back
// into `Stage`, and the parameterized variants would require runtime-format
// parsing that strum cannot generate.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Hash,
    strum::Display,
    strum::IntoStaticStr,
    strum::EnumDiscriminants,
)]
#[strum_discriminants(name(StageKind))]
pub enum Stage {
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
    #[strum(to_string = "Waiting to implement")]
    WaitingToImplement,
    #[strum(to_string = "Updating plan")]
    RepoStateUpdateRunning,
    #[strum(to_string = "Sharding")]
    ShardingRunning,
    #[strum(to_string = "Skip Confirmation")]
    SkipToImplPending,
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
    /// Operator decision point after a successful final validation suggests
    /// Dreaming. Persisted separately from the validator verdict so resume
    /// shows the same decision instead of re-running final validation.
    #[strum(to_string = "Dreaming Pending")]
    DreamingPending,
    /// Future noninteractive memory consolidation pass.
    #[strum(to_string = "Dreaming Round {0}")]
    Dreaming(u32),
    /// Behavior-preserving cleanup pass between loop convergence and FinalValidation.
    /// The stored round matches the coder round whose work is being simplified.
    #[strum(to_string = "Simplification Round {0}")]
    Simplification(u32),
    #[strum(to_string = "Done")]
    Done,
    #[strum(to_string = "Cancelled")]
    Cancelled,
    #[strum(to_string = "Blocked")]
    BlockedNeedsUser,
}
impl Stage {
    /// Project this persisted stage down to the slim lifecycle stage.
    pub fn to_slim_stage(self) -> crate::lifecycle::Stage {
        use crate::lifecycle::Stage as SlimStage;
        match self {
            Stage::IdeaInput | Stage::BrainstormRunning => SlimStage::Idea,
            Stage::SpecReviewRunning | Stage::SpecReviewPaused => SlimStage::Spec,
            Stage::PlanningRunning
            | Stage::PlanReviewRunning
            | Stage::PlanReviewPaused
            | Stage::ShardingRunning
            | Stage::RepoStateUpdateRunning
            | Stage::WaitingToImplement
            | Stage::SkipToImplPending => SlimStage::Plan,
            Stage::ImplementationRound(r)
            | Stage::BuilderRecovery(r)
            | Stage::BuilderRecoveryPlanReview(r)
            | Stage::BuilderRecoverySharding(r) => SlimStage::Implementation(r),
            Stage::ReviewRound(r) | Stage::Simplification(r) => SlimStage::Review(r),
            Stage::FinalValidation(_) | Stage::Dreaming(_) | Stage::DreamingPending => {
                SlimStage::Finalization
            }
            Stage::Done => SlimStage::Done,
            Stage::Cancelled => SlimStage::Cancelled,
            Stage::BlockedNeedsUser | Stage::GitGuardPending => SlimStage::Plan,
        }
    }

    /// Pick a representative persisted stage to land on when rewinding to `target`.
    pub fn from_slim_stage(target: crate::lifecycle::Stage) -> Self {
        use crate::lifecycle::Stage as SlimStage;
        match target {
            SlimStage::Idea => Stage::IdeaInput,
            SlimStage::Spec => Stage::SpecReviewRunning,
            SlimStage::Plan => Stage::PlanningRunning,
            SlimStage::Implementation(r) => Stage::ImplementationRound(r),
            SlimStage::Review(r) => Stage::ReviewRound(r),
            SlimStage::Finalization => Stage::FinalValidation(1),
            SlimStage::Done => Stage::Done,
            SlimStage::Cancelled => Stage::Cancelled,
        }
    }

    fn round(self) -> Option<u32> {
        match self {
            Stage::ImplementationRound(round)
            | Stage::ReviewRound(round)
            | Stage::BuilderRecovery(round)
            | Stage::BuilderRecoveryPlanReview(round)
            | Stage::BuilderRecoverySharding(round)
            | Stage::FinalValidation(round)
            | Stage::Dreaming(round)
            | Stage::Simplification(round) => Some(round),
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
            Stage::IdeaInput => "Idea Input".to_string(),
            Stage::BrainstormRunning => "Brainstorming".to_string(),
            Stage::SpecReviewRunning | Stage::SpecReviewPaused => "Spec Review".to_string(),
            Stage::PlanningRunning => "Planning".to_string(),
            Stage::PlanReviewRunning | Stage::PlanReviewPaused => "Plan Review".to_string(),
            Stage::WaitingToImplement => "Waiting to implement".to_string(),
            Stage::RepoStateUpdateRunning => "Updating plan".to_string(),
            Stage::ShardingRunning => "Sharding".to_string(),
            Stage::ImplementationRound(r) => format!("Round {r} Coder"),
            Stage::ReviewRound(r) => format!("Round {r} Reviewer"),
            Stage::BuilderRecovery(_) => "Builder Recovery".to_string(),
            Stage::BuilderRecoveryPlanReview(_) => "Recovery Plan Review".to_string(),
            Stage::BuilderRecoverySharding(_) => "Recovery Sharding".to_string(),
            Stage::Done => "Done".to_string(),
            Stage::Cancelled => "Cancelled".to_string(),
            Stage::BlockedNeedsUser => "Blocked".to_string(),
            Stage::SkipToImplPending => "Skip Confirmation".to_string(),
            Stage::GitGuardPending => "Guard Decision".to_string(),
            Stage::FinalValidation(_) => "Final Validation".to_string(),
            Stage::DreamingPending | Stage::Dreaming(_) => "Dreaming".to_string(),
            Stage::Simplification(_) => "Simplification".to_string(),
        }
    }
    /// Returns true if a transition from `self` to `target` is valid.
    pub fn can_transition_to(&self, target: &Stage) -> bool {
        stage_graph::can_transition(self, target)
    }
}
