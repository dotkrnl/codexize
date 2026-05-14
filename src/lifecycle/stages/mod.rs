//! Concrete [`Stage`](super::Stage) implementations.
//!
//! One module per pipeline stage. Each module exports a unit struct named
//! `<StageName>Stage` plus its `impl Stage` block; nothing here is wired
//! into a [`StageRegistry`](super::StageRegistry) yet — Step 3 owns the
//! registration step. The structs and their tests exist now so the trait
//! contract is exercised before the FSM scheduler turns them on.
pub mod brainstorm;
pub mod coder;
pub mod plan_review;
pub mod planning;
pub mod recovery;
pub mod reviewer;
pub mod sharding;
pub mod spec_review;

pub use brainstorm::BrainstormStage;
pub use coder::CoderStage;
pub use plan_review::PlanReviewStage;
pub use planning::PlanningStage;
pub use recovery::RecoveryStage;
pub use reviewer::ReviewerStage;
pub use sharding::ShardingStage;
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
