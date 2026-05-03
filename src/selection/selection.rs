use super::config::SelectionPhase;
use super::ranking::{VersionIndex, candidate_pool_weights};
use super::types::{CachedModel, VendorKind};
use super::vendor::{is_cheap_eligible, is_tough_eligible};
use crate::adapters::EffortLevel;
use std::cmp::Ordering;
use std::ops::Deref;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

#[cfg(test)]
static TEST_SAMPLE_SEED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionWarning {
    CheapFallback {
        phase: SelectionPhase,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionOutcome<'a> {
    pub model: &'a CachedModel,
    pub warning: Option<SelectionWarning>,
}

impl<'a> SelectionOutcome<'a> {
    fn ok(model: &'a CachedModel) -> Self {
        Self {
            model,
            warning: None,
        }
    }

    fn with_warning(model: &'a CachedModel, warning: SelectionWarning) -> Self {
        Self {
            model,
            warning: Some(warning),
        }
    }
}

impl Deref for SelectionOutcome<'_> {
    type Target = CachedModel;

    fn deref(&self) -> &Self::Target {
        self.model
    }
}

/// Score `candidates` with the candidate-pool sampler and weighted-sample
/// the result. Hard pre-filters (vendor, eligibility tier, retry exclusion,
/// diversity tier) MUST already be applied to the slice. The pool scorer
/// drops `quota_percent == Some(0)` and rows missing the ipbr phase score
/// for `phase`; if every candidate is dropped this returns `None`, which
/// callers surface as the existing no-eligible-model condition.
fn pool_pick<'a>(candidates: &[&'a CachedModel], phase: SelectionPhase) -> Option<&'a CachedModel> {
    if candidates.is_empty() {
        return None;
    }

    let weights = candidate_pool_weights(candidates, phase);
    let mut weighted: Vec<(&'a CachedModel, f64)> = candidates
        .iter()
        .copied()
        .zip(weights.iter().copied())
        .filter(|(_, weight)| *weight > 0.0)
        .collect();

    if weighted.is_empty() {
        return None;
    }

    weighted.sort_by(|(left_model, left_weight), (right_model, right_weight)| {
        right_weight
            .partial_cmp(left_weight)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left_model.name.cmp(&right_model.name))
    });

    weighted_sample(&weighted)
}

/// Select a model for the given phase using the candidate-pool scorer.
///
/// `vendor_filter`: hard inclusion filter applied before the pool scorer —
/// vendor preferences are not multiplied into the post-softmax weights.
pub fn pick_for_phase<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<VendorKind>,
    _version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    let candidates: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| vendor_filter.is_none_or(|v| v == model.vendor))
        .collect();

    pool_pick(&candidates, phase)
}

/// Effort-aware variant of [`pick_for_phase`].
///
/// For [`EffortLevel::Tough`], samples over the tough-eligible subset
/// (Claude-opus and all Codex models) using the candidate-pool scorer.
/// Falls back to the full slice only when the tough-eligible pool collapses
/// — for example, every tough-eligible model is exhausted (`quota_percent
/// == Some(0)`) or unranked for the phase — so the run still launches.
///
/// For [`EffortLevel::Low`] and [`EffortLevel::Normal`], delegates straight
/// to [`pick_for_phase`].
pub fn pick_for_phase_with_effort<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<VendorKind>,
    version_index: &VersionIndex,
    effort: EffortLevel,
    cheap: bool,
) -> Option<SelectionOutcome<'a>> {
    if cheap {
        let eligible: Vec<&'a CachedModel> = models
            .iter()
            .filter(|model| is_cheap_eligible(model))
            .filter(|model| vendor_filter.is_none_or(|v| v == model.vendor))
            .collect();

        if let Some(chosen) = pool_pick(&eligible, phase) {
            return Some(SelectionOutcome::ok(chosen));
        }

        return pick_for_phase(models, phase, vendor_filter, version_index).map(|model| {
            SelectionOutcome::with_warning(
                model,
                SelectionWarning::CheapFallback {
                    phase,
                    reason: "no_eligible_with_quota",
                },
            )
        });
    }

    match effort {
        EffortLevel::Low | EffortLevel::Normal => {
            return pick_for_phase(models, phase, vendor_filter, version_index)
                .map(SelectionOutcome::ok);
        }
        EffortLevel::Tough => {}
    }

    let eligible: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| is_tough_eligible(model))
        .filter(|model| vendor_filter.is_none_or(|v| v == model.vendor))
        .collect();

    if let Some(chosen) = pool_pick(&eligible, phase) {
        return Some(SelectionOutcome::ok(chosen));
    }

    // Degraded fallback: no tough-eligible model has any pool weight at all
    // (all exhausted or unranked). Fall back to the full slice so the run
    // still launches.
    pick_for_phase(models, phase, vendor_filter, version_index).map(SelectionOutcome::ok)
}

/// Select a model for review with unused-vendor preference.
///
/// Tier 1 prefers different vendor *and* different model. Tier 2 falls back
/// to any unused model. Each tier is a hard filter; the candidate-pool
/// scorer is applied within the tier.
pub fn select_for_review<'a>(
    models: &'a [CachedModel],
    used_vendors: &[VendorKind],
    used_models: &[(VendorKind, String)],
    _version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    let tier_1: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| {
            !used_vendors.contains(&model.vendor)
                && !used_models.contains(&(model.vendor, model.name.clone()))
        })
        .collect();
    if let Some(chosen) = pool_pick(&tier_1, SelectionPhase::Review) {
        return Some(chosen);
    }

    let tier_2: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| !used_models.contains(&(model.vendor, model.name.clone())))
        .collect();
    pool_pick(&tier_2, SelectionPhase::Review)
}

/// Effort-aware variant of [`select_for_review`].
///
/// For [`EffortLevel::Tough`], applies [`is_tough_eligible`] to each
/// diversity tier (fresh-vendor, fresh-model, then any-eligible) before
/// running the pool scorer. Only when every tough-eligible model is unable
/// to score in *all three* tiers does it degrade to the unfiltered
/// selection — eligibility dominates diversity. In particular, if the only
/// tough-eligible model with quota was already used by the coder, the
/// reviewer reuses it rather than picking a fresh sonnet or Kimi.
///
/// For [`EffortLevel::Low`] and [`EffortLevel::Normal`], delegates to
/// [`select_for_review`].
pub fn select_for_review_with_effort<'a>(
    models: &'a [CachedModel],
    used_vendors: &[VendorKind],
    used_models: &[(VendorKind, String)],
    version_index: &VersionIndex,
    effort: EffortLevel,
    cheap: bool,
) -> Option<SelectionOutcome<'a>> {
    if cheap {
        let eligible: Vec<&'a CachedModel> = models
            .iter()
            .filter(|model| is_cheap_eligible(model))
            .collect();
        if let Some(chosen) = select_for_review_from_eligible(&eligible, used_vendors, used_models)
        {
            return Some(SelectionOutcome::ok(chosen));
        }

        return select_for_review(models, used_vendors, used_models, version_index).map(|model| {
            SelectionOutcome::with_warning(
                model,
                SelectionWarning::CheapFallback {
                    phase: SelectionPhase::Review,
                    reason: "no_eligible_with_quota",
                },
            )
        });
    }

    match effort {
        EffortLevel::Low | EffortLevel::Normal => {
            return select_for_review(models, used_vendors, used_models, version_index)
                .map(SelectionOutcome::ok);
        }
        EffortLevel::Tough => {}
    }

    let eligible: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| is_tough_eligible(model))
        .collect();

    select_for_review_from_eligible(&eligible, used_vendors, used_models)
        .map(SelectionOutcome::ok)
        .or_else(|| {
            select_for_review(models, used_vendors, used_models, version_index)
                .map(SelectionOutcome::ok)
        })
}

fn select_for_review_from_eligible<'a>(
    eligible: &[&'a CachedModel],
    used_vendors: &[VendorKind],
    used_models: &[(VendorKind, String)],
) -> Option<&'a CachedModel> {
    // Tier 1: eligible AND fresh-vendor AND fresh-model.
    let tier_1: Vec<&'a CachedModel> = eligible
        .iter()
        .copied()
        .filter(|model| {
            !used_vendors.contains(&model.vendor)
                && !used_models.contains(&(model.vendor, model.name.clone()))
        })
        .collect();
    if let Some(chosen) = pool_pick(&tier_1, SelectionPhase::Review) {
        return Some(chosen);
    }

    // Tier 2: eligible AND fresh-model (any vendor).
    let tier_2: Vec<&'a CachedModel> = eligible
        .iter()
        .copied()
        .filter(|model| !used_models.contains(&(model.vendor, model.name.clone())))
        .collect();
    if let Some(chosen) = pool_pick(&tier_2, SelectionPhase::Review) {
        return Some(chosen);
    }

    // Tier 3: any eligible model, even if used by the coder.
    // This is "eligibility dominates diversity": prefer reusing Claude-opus
    // over a fresh sonnet/Kimi when no fresh eligible model is available.
    pool_pick(eligible, SelectionPhase::Review)
}

/// Select a model excluding a list of models. The `last_failed_vendor`
/// diversity boost from the legacy probability chain is intentionally
/// dropped: spec §5.3 / §6 forbid post-softmax policy multipliers.
pub fn select_excluding<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    excluded: &[(VendorKind, String)],
    _last_failed_vendor: Option<VendorKind>,
    _version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    if models.is_empty() {
        return None;
    }

    let candidates: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| !excluded.contains(&(model.vendor, model.name.clone())))
        .collect();

    pool_pick(&candidates, phase)
}

/// Weighted random sampling from candidates.
/// Returns None if candidates is empty or all weights are zero.
fn weighted_sample<'a>(candidates: &[(&'a CachedModel, f64)]) -> Option<&'a CachedModel> {
    if candidates.is_empty() {
        return None;
    }

    let total: f64 = candidates.iter().map(|(_, weight)| *weight).sum();

    if total <= 0.0 {
        return candidates.first().map(|(model, _)| *model);
    }

    let seed = sample_seed() as f64;
    let r = (seed % 1_000_000.0) / 1_000_000.0 * total;

    let mut cumulative = 0.0;
    for (model, weight) in candidates.iter() {
        cumulative += *weight;
        if r < cumulative {
            return Some(model);
        }
    }

    candidates.last().map(|(model, _)| *model)
}

fn sample_seed() -> u64 {
    #[cfg(test)]
    {
        let seeded = TEST_SAMPLE_SEED.load(AtomicOrdering::Relaxed);
        if seeded != 0 {
            return seeded;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
}

#[cfg(test)]
mod tests_mod;
