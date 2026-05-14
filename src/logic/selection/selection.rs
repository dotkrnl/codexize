use super::config::SelectionPhase;
use super::ranking::candidate_pool_weights;
use super::subscription::{is_cheap_eligible, is_tough_eligible};
use super::types::{CachedModel, SubscriptionKind};
use crate::adapters::EffortLevel;
use std::cmp::Ordering;
use std::ops::Deref;
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
///
/// Per spec §"Selection algorithm" (operator decision, 2026-05-09): step
/// 1 is a *weighted sampler*, not a deterministic max-by. The dashboard
/// phase score and the row's effective quota across enabled providers feed
/// `candidate_pool_weights` together — free / quota_disabled providers
/// therefore dominate the sampler probability at full headroom, while a
/// low-quota row keeps a small but non-zero chance of being explored. See
/// `ranking::effective_row_quota` for the quota lookup that backs this.
fn pool_pick<'a>(
    candidates: &[&'a CachedModel],
    phase: SelectionPhase,
    sample_seed: u64,
) -> Option<&'a CachedModel> {
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
    weighted_sample(&weighted, sample_seed)
}
/// Select a model for the given phase using the candidate-pool scorer.
///
/// `vendor_filter`: hard inclusion filter applied before the pool scorer —
/// vendor preferences are not multiplied into the post-softmax weights.
#[cfg(test)]
pub fn pick_for_phase(
    models: &[CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<SubscriptionKind>,
) -> Option<&CachedModel> {
    pick_for_phase_with_seed(models, phase, vendor_filter, test_sample_seed())
}
pub fn pick_for_phase_with_seed<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<SubscriptionKind>,
    sample_seed: u64,
) -> Option<&'a CachedModel> {
    let candidates: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| vendor_filter.is_none_or(|v| v == model.subscription))
        .collect();
    pool_pick(&candidates, phase, sample_seed)
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
#[cfg(test)]
pub fn pick_for_phase_with_effort<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<SubscriptionKind>,
    effort: EffortLevel,
    cheap: bool,
) -> Option<SelectionOutcome<'a>> {
    pick_for_phase_with_effort_and_seed(
        models,
        phase,
        vendor_filter,
        effort,
        cheap,
        test_sample_seed(),
    )
}
pub fn pick_for_phase_with_effort_and_seed<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<SubscriptionKind>,
    effort: EffortLevel,
    cheap: bool,
    sample_seed: u64,
) -> Option<SelectionOutcome<'a>> {
    if cheap {
        let eligible: Vec<&'a CachedModel> = models
            .iter()
            .filter(|model| is_cheap_eligible(model))
            .filter(|model| vendor_filter.is_none_or(|v| v == model.subscription))
            .collect();
        if let Some(chosen) = pool_pick(&eligible, phase, sample_seed) {
            return Some(SelectionOutcome::ok(chosen));
        }
        return pick_for_phase_with_seed(models, phase, vendor_filter, sample_seed).map(|model| {
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
            return pick_for_phase_with_seed(models, phase, vendor_filter, sample_seed)
                .map(SelectionOutcome::ok);
        }
        EffortLevel::Tough => {}
    }
    let eligible: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| is_tough_eligible(model))
        .filter(|model| vendor_filter.is_none_or(|v| v == model.subscription))
        .collect();
    if let Some(chosen) = pool_pick(&eligible, phase, sample_seed) {
        return Some(SelectionOutcome::ok(chosen));
    }
    // Degraded fallback: no tough-eligible model has any pool weight at all
    // (all exhausted or unranked). Fall back to the full slice so the run
    // still launches.
    pick_for_phase_with_seed(models, phase, vendor_filter, sample_seed).map(SelectionOutcome::ok)
}
/// Select a model for review with unused-vendor preference.
///
/// Tier 1 prefers different vendor *and* different model. Tier 2 falls back
/// to any unused model. Each tier is a hard filter; the candidate-pool
/// scorer is applied within the tier.
#[cfg(test)]
pub fn select_for_review<'a>(
    models: &'a [CachedModel],
    used_vendors: &[SubscriptionKind],
    used_models: &[(SubscriptionKind, String)],
) -> Option<&'a CachedModel> {
    select_for_review_with_seed(models, used_vendors, used_models, test_sample_seed())
}
pub fn select_for_review_with_seed<'a>(
    models: &'a [CachedModel],
    used_vendors: &[SubscriptionKind],
    used_models: &[(SubscriptionKind, String)],
    sample_seed: u64,
) -> Option<&'a CachedModel> {
    let tier_1: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| {
            !used_vendors.contains(&model.subscription)
                && !used_models.contains(&(model.subscription, model.name.clone()))
        })
        .collect();
    if let Some(chosen) = pool_pick(&tier_1, SelectionPhase::Review, sample_seed) {
        return Some(chosen);
    }
    let tier_2: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| !used_models.contains(&(model.subscription, model.name.clone())))
        .collect();
    pool_pick(&tier_2, SelectionPhase::Review, sample_seed)
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
#[cfg(test)]
pub fn select_for_review_with_effort<'a>(
    models: &'a [CachedModel],
    used_vendors: &[SubscriptionKind],
    used_models: &[(SubscriptionKind, String)],
    effort: EffortLevel,
    cheap: bool,
) -> Option<SelectionOutcome<'a>> {
    select_for_review_with_effort_and_seed(
        models,
        used_vendors,
        used_models,
        effort,
        cheap,
        test_sample_seed(),
    )
}
pub fn select_for_review_with_effort_and_seed<'a>(
    models: &'a [CachedModel],
    used_vendors: &[SubscriptionKind],
    used_models: &[(SubscriptionKind, String)],
    effort: EffortLevel,
    cheap: bool,
    sample_seed: u64,
) -> Option<SelectionOutcome<'a>> {
    if cheap {
        let eligible: Vec<&'a CachedModel> = models
            .iter()
            .filter(|model| is_cheap_eligible(model))
            .collect();
        if let Some(chosen) =
            select_for_review_from_eligible(&eligible, used_vendors, used_models, sample_seed)
        {
            return Some(SelectionOutcome::ok(chosen));
        }
        return select_for_review_with_seed(models, used_vendors, used_models, sample_seed).map(
            |model| {
                SelectionOutcome::with_warning(
                    model,
                    SelectionWarning::CheapFallback {
                        phase: SelectionPhase::Review,
                        reason: "no_eligible_with_quota",
                    },
                )
            },
        );
    }
    match effort {
        EffortLevel::Low | EffortLevel::Normal => {
            return select_for_review_with_seed(models, used_vendors, used_models, sample_seed)
                .map(SelectionOutcome::ok);
        }
        EffortLevel::Tough => {}
    }
    let eligible: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| is_tough_eligible(model))
        .collect();
    select_for_review_from_eligible(&eligible, used_vendors, used_models, sample_seed)
        .map(SelectionOutcome::ok)
        .or_else(|| {
            select_for_review_with_seed(models, used_vendors, used_models, sample_seed)
                .map(SelectionOutcome::ok)
        })
}
fn select_for_review_from_eligible<'a>(
    eligible: &[&'a CachedModel],
    used_vendors: &[SubscriptionKind],
    used_models: &[(SubscriptionKind, String)],
    sample_seed: u64,
) -> Option<&'a CachedModel> {
    // Tier 1: eligible AND fresh-vendor AND fresh-model.
    let tier_1: Vec<&'a CachedModel> = eligible
        .iter()
        .copied()
        .filter(|model| {
            !used_vendors.contains(&model.subscription)
                && !used_models.contains(&(model.subscription, model.name.clone()))
        })
        .collect();
    if let Some(chosen) = pool_pick(&tier_1, SelectionPhase::Review, sample_seed) {
        return Some(chosen);
    }
    // Tier 2: eligible AND fresh-model (any vendor).
    let tier_2: Vec<&'a CachedModel> = eligible
        .iter()
        .copied()
        .filter(|model| !used_models.contains(&(model.subscription, model.name.clone())))
        .collect();
    if let Some(chosen) = pool_pick(&tier_2, SelectionPhase::Review, sample_seed) {
        return Some(chosen);
    }
    // Tier 3: any eligible model, even if used by the coder.
    // This is "eligibility dominates diversity": prefer reusing Claude-opus
    // over a fresh sonnet/Kimi when no fresh eligible model is available.
    pool_pick(eligible, SelectionPhase::Review, sample_seed)
}
/// Select a model excluding a list of models. `last_failed_vendor` does not
/// affect weights: spec §5.3 / §6 forbid post-softmax policy multipliers.
#[cfg(test)]
pub fn select_excluding<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    excluded: &[(SubscriptionKind, String)],
    _last_failed_vendor: Option<SubscriptionKind>,
) -> Option<&'a CachedModel> {
    select_excluding_with_seed(models, phase, excluded, None, test_sample_seed())
}
pub fn select_excluding_with_seed<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    excluded: &[(SubscriptionKind, String)],
    _last_failed_vendor: Option<SubscriptionKind>,
    sample_seed: u64,
) -> Option<&'a CachedModel> {
    if models.is_empty() {
        return None;
    }
    let candidates: Vec<&'a CachedModel> = models
        .iter()
        .filter(|model| !excluded.contains(&(model.subscription, model.name.clone())))
        .collect();
    pool_pick(&candidates, phase, sample_seed)
}
/// Weighted random sampling from candidates.
/// Returns None if candidates is empty or all weights are zero.
fn weighted_sample<'a>(
    candidates: &[(&'a CachedModel, f64)],
    sample_seed: u64,
) -> Option<&'a CachedModel> {
    if candidates.is_empty() {
        return None;
    }
    let total: f64 = candidates.iter().map(|(_, weight)| *weight).sum();
    if total <= 0.0 {
        return candidates.first().map(|(model, _)| *model);
    }
    let seed = sample_seed as f64;
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
#[cfg(test)]
fn test_sample_seed() -> u64 {
    let seeded = TEST_SAMPLE_SEED.load(AtomicOrdering::Relaxed);
    if seeded != 0 {
        return seeded;
    }
    1
}
#[cfg(test)]
mod tests_mod;
