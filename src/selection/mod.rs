pub mod assemble;
pub mod quota;

pub use crate::logic::selection::{
    CachedModel, IpbrPhaseScores, QuotaError, ScoreSource, VendorKind,
};
pub use crate::logic::selection::{config, display, ranking, types, vendor};
pub use config::*;

#[allow(clippy::module_inception)]
pub mod selection {
    use crate::adapters::EffortLevel;
    use crate::logic::selection::{SelectionPhase, selection as pure};
    use crate::logic::selection::{
        ranking::VersionIndex,
        types::{CachedModel, VendorKind},
    };

    fn sample_seed() -> u64 {
        chrono::Utc::now().timestamp_subsec_nanos() as u64
    }

    pub fn pick_for_phase<'a>(
        models: &'a [CachedModel],
        phase: SelectionPhase,
        vendor_filter: Option<VendorKind>,
        version_index: &VersionIndex,
    ) -> Option<&'a CachedModel> {
        pure::pick_for_phase_with_seed(models, phase, vendor_filter, version_index, sample_seed())
    }

    pub fn pick_for_phase_with_effort<'a>(
        models: &'a [CachedModel],
        phase: SelectionPhase,
        vendor_filter: Option<VendorKind>,
        version_index: &VersionIndex,
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<pure::SelectionOutcome<'a>> {
        pure::pick_for_phase_with_effort_and_seed(
            models,
            phase,
            vendor_filter,
            version_index,
            effort,
            cheap,
            sample_seed(),
        )
    }

    pub fn select_for_review<'a>(
        models: &'a [CachedModel],
        used_vendors: &[VendorKind],
        used_models: &[(VendorKind, String)],
        version_index: &VersionIndex,
    ) -> Option<&'a CachedModel> {
        pure::select_for_review_with_seed(
            models,
            used_vendors,
            used_models,
            version_index,
            sample_seed(),
        )
    }

    pub fn select_for_review_with_effort<'a>(
        models: &'a [CachedModel],
        used_vendors: &[VendorKind],
        used_models: &[(VendorKind, String)],
        version_index: &VersionIndex,
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<pure::SelectionOutcome<'a>> {
        pure::select_for_review_with_effort_and_seed(
            models,
            used_vendors,
            used_models,
            version_index,
            effort,
            cheap,
            sample_seed(),
        )
    }

    pub fn select_excluding<'a>(
        models: &'a [CachedModel],
        phase: SelectionPhase,
        excluded: &[(VendorKind, String)],
        last_failed_vendor: Option<VendorKind>,
        version_index: &VersionIndex,
    ) -> Option<&'a CachedModel> {
        pure::select_excluding_with_seed(
            models,
            phase,
            excluded,
            last_failed_vendor,
            version_index,
            sample_seed(),
        )
    }

    pub use pure::{SelectionOutcome, SelectionWarning};
}
