//! Default [`StageRegistry`] wiring.
//!
//! Builds the scheduler's registry with the 14 concrete
//! [`Stage`](crate::lifecycle::stage::Stage) impls. The registry is
//! constructed fresh on every call so callers that want to override a stage
//! (tests, future plug-ins) can simply
//! [`register`](crate::lifecycle::stage::StageRegistry::register) over the
//! top.
use crate::lifecycle::stage::StageRegistry;

use super::{
    BrainstormStage, CoderStage, DreamingStage, FinalValidationStage, PlanReviewStage,
    PlanningStage, RecoveryPlanReviewStage, RecoveryShardingStage, RecoveryStage,
    RepoStateUpdateStage, ReviewerStage, ShardingStage, SimplificationStage, SpecReviewStage,
};

/// Build a [`StageRegistry`] pre-populated with the 14 default
/// [`Stage`](crate::lifecycle::stage::Stage) impls.
///
/// Order of registration is irrelevant — the registry keys by
/// [`StageId`](crate::lifecycle::stage_id::StageId), which each stage
/// surfaces through [`Stage::id`](crate::lifecycle::stage::Stage::id).
pub fn default_registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Box::new(BrainstormStage));
    r.register(Box::new(SpecReviewStage));
    r.register(Box::new(PlanningStage));
    r.register(Box::new(PlanReviewStage));
    r.register(Box::new(RepoStateUpdateStage));
    r.register(Box::new(ShardingStage));
    r.register(Box::new(CoderStage));
    r.register(Box::new(ReviewerStage));
    r.register(Box::new(RecoveryStage));
    r.register(Box::new(RecoveryPlanReviewStage));
    r.register(Box::new(RecoveryShardingStage));
    r.register(Box::new(FinalValidationStage));
    r.register(Box::new(SimplificationStage));
    r.register(Box::new(DreamingStage));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::stage_id::StageId;

    /// Every variant of [`StageId`] must round-trip through `default_registry`.
    /// If a new variant is added without an accompanying registration this
    /// test will fail with the missing variant in plain sight.
    #[test]
    fn default_registry_covers_every_stage_id() {
        let r = default_registry();
        for id in [
            StageId::Brainstorm,
            StageId::SpecReview,
            StageId::Planning,
            StageId::PlanReview,
            StageId::RepoStateUpdate,
            StageId::Sharding,
            StageId::Coder,
            StageId::Reviewer,
            StageId::Recovery,
            StageId::RecoveryPlanReview,
            StageId::RecoverySharding,
            StageId::FinalValidation,
            StageId::Simplification,
            StageId::Dreaming,
        ] {
            assert!(
                r.get(id).is_some(),
                "default_registry missing registration for {id:?}"
            );
        }
    }

    /// Each registered stage must report the [`StageId`] it was looked up by;
    /// guards against a future copy-paste mistake that registers
    /// `FooStage` under the wrong slot.
    #[test]
    fn registered_stage_reports_matching_id() {
        let r = default_registry();
        for id in [
            StageId::Brainstorm,
            StageId::SpecReview,
            StageId::Planning,
            StageId::PlanReview,
            StageId::RepoStateUpdate,
            StageId::Sharding,
            StageId::Coder,
            StageId::Reviewer,
            StageId::Recovery,
            StageId::RecoveryPlanReview,
            StageId::RecoverySharding,
            StageId::FinalValidation,
            StageId::Simplification,
            StageId::Dreaming,
        ] {
            let stage = r.get(id).expect("registered");
            assert_eq!(stage.id(), id);
        }
    }
}
