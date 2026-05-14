//! Concrete [`Stage`](super::Stage) implementations.
//!
//! One module per pipeline stage. Each module exports a unit struct named
//! `<StageName>Stage` plus its `impl Stage` block. The default registry wires
//! these stages into the scheduler-facing lifecycle pipeline.
pub mod brainstorm;
pub mod coder;
pub mod dreaming;
pub mod final_validation;
pub mod plan_review;
pub mod planning;
pub mod recovery;
pub mod recovery_plan_review;
pub mod recovery_sharding;
mod registry;
pub mod repo_state_update;
pub mod reviewer;
pub mod sharding;
pub mod simplification;
pub mod spec_review;

pub use brainstorm::BrainstormStage;
pub use coder::CoderStage;
pub use dreaming::DreamingStage;
pub use final_validation::FinalValidationStage;
pub use plan_review::PlanReviewStage;
pub use planning::PlanningStage;
pub use recovery::RecoveryStage;
pub use recovery_plan_review::RecoveryPlanReviewStage;
pub use recovery_sharding::RecoveryShardingStage;
pub use registry::default_registry;
pub use repo_state_update::RepoStateUpdateStage;
pub use reviewer::ReviewerStage;
pub use sharding::ShardingStage;
pub use simplification::SimplificationStage;
pub use spec_review::SpecReviewStage;

use crate::lifecycle::fsm::Outcome;
use crate::lifecycle::stage::StageCtx;
use crate::lifecycle::stage_id::StageId;

/// Highest `attempt + 1` seen for this `(stage, task, round)` in
/// `ctx.prior_runs`, or `1` if none. Shared by every Stage impl that needs
/// to construct a fresh `StageSpec`.
pub(crate) fn next_attempt(
    ctx: &StageCtx<'_>,
    stage: StageId,
    task: Option<u32>,
    round: u32,
) -> u32 {
    ctx.prior_runs
        .iter()
        .filter(|r| r.stage_id == stage && r.task_id == task && r.round == round)
        .map(|r| r.attempt)
        .max()
        .map(|a| a.saturating_add(1))
        .unwrap_or(1)
}

/// True when any prior run for this `(stage, task, round)` is `Outcome::Done`.
pub(crate) fn has_succeeded(
    ctx: &StageCtx<'_>,
    stage: StageId,
    task: Option<u32>,
    round: u32,
) -> bool {
    ctx.prior_runs.iter().any(|r| {
        r.stage_id == stage
            && r.task_id == task
            && r.round == round
            && r.outcome == Some(Outcome::Done)
    })
}
