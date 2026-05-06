use serde::{Deserialize, Serialize};
#[path = "phase_graph.rs"]
mod phase_graph;
#[cfg(test)]
#[path = "phase_tests.rs"]
mod phase_tests;
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
    Hash,
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
        phase_graph::can_transition(self, target)
    }
}
