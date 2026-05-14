//! Flat `crate::selection::*` public surface.
//!
//! This module is the intentional flat alias for the layered selection
//! homes — pure logic in [`crate::logic::selection`] and IO loaders in
//! [`crate::data::selection_quota`] / [`crate::data::selection_assembly`].
//! Keeping the alias lets `main.rs`, integration tests, and a future
//! server-mode binary import everything selection-shaped from one root
//! path; new logic/data callers should still prefer the layered names.
pub use crate::logic::selection::{
    CachedModel, Candidate, CliKind, IpbrStageScores, ModelRow, QuotaError, ScoreSource,
    SubscriptionKind,
};
pub use crate::logic::selection::{config, display, ranking, subscription, types};
pub use config::*;
#[allow(clippy::module_inception)]
pub mod selection {
    use crate::data::adapters::EffortLevel;
    use crate::logic::selection::types::{CachedModel, SubscriptionKind};
    use crate::logic::selection::{SelectionStage, selection as pure};
    fn sample_seed() -> u64 {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).expect("getrandom failed");
        u64::from_ne_bytes(buf)
    }
    pub fn pick_for_stage(
        models: &[CachedModel],
        stage: SelectionStage,
        vendor_filter: Option<SubscriptionKind>,
    ) -> Option<&CachedModel> {
        pure::pick_for_stage_with_seed(models, stage, vendor_filter, sample_seed())
    }
    pub fn pick_for_stage_with_effort<'a>(
        models: &'a [CachedModel],
        stage: SelectionStage,
        vendor_filter: Option<SubscriptionKind>,
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<pure::SelectionOutcome<'a>> {
        pure::pick_for_stage_with_effort_and_seed(
            models,
            stage,
            vendor_filter,
            effort,
            cheap,
            sample_seed(),
        )
    }
    pub fn select_for_review<'a>(
        models: &'a [CachedModel],
        used_vendors: &[SubscriptionKind],
        used_models: &[(SubscriptionKind, String)],
    ) -> Option<&'a CachedModel> {
        // Lifetime is explicit so callers can hold the picked model alive for
        // the lifetime of `models` independent of `used_*`.
        pure::select_for_review_with_seed(models, used_vendors, used_models, sample_seed())
    }
    pub fn select_for_review_with_effort<'a>(
        models: &'a [CachedModel],
        used_vendors: &[SubscriptionKind],
        used_models: &[(SubscriptionKind, String)],
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<pure::SelectionOutcome<'a>> {
        pure::select_for_review_with_effort_and_seed(
            models,
            used_vendors,
            used_models,
            effort,
            cheap,
            sample_seed(),
        )
    }
    pub fn select_excluding<'a>(
        models: &'a [CachedModel],
        stage: SelectionStage,
        excluded: &[(SubscriptionKind, String)],
        last_failed_vendor: Option<SubscriptionKind>,
    ) -> Option<&'a CachedModel> {
        pure::select_excluding_with_seed(models, stage, excluded, last_failed_vendor, sample_seed())
    }
    pub use pure::{SelectionOutcome, SelectionWarning};
}
