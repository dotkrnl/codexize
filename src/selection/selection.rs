use super::config::{SELECTION_CONFIG, SelectionPhase};
use super::ranking::{VersionIndex, selection_probability};
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

/// Select a model for the given phase using relative cutoff + weighted random.
///
/// `vendor_filter`: Optional hard inclusion filter. If `Some(v)`, only models
/// from vendor `v` are considered.
///
/// `max_prob` for the cutoff is computed over the full unfiltered slice to
/// ensure filtering doesn't accidentally lower the cutoff and admit more models.
pub fn pick_for_phase<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<VendorKind>,
    version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    if models.is_empty() {
        return None;
    }

    // Compute probabilities for all models
    let probabilities: Vec<f64> = models
        .iter()
        .map(|m| selection_probability(m, phase, version_index))
        .collect();

    // Find max probability across ALL models (before vendor filtering)
    let max_prob = probabilities.iter().copied().fold(0.0_f64, f64::max);

    if max_prob <= 0.0 {
        return None;
    }

    // Apply relative cutoff
    let cutoff = max_prob * SELECTION_CONFIG.min_selection_probability_ratio;

    // Build candidate list: apply cutoff and vendor filter
    let mut candidates: Vec<(&CachedModel, f64)> = models
        .iter()
        .zip(probabilities.iter())
        .filter(|(model, prob)| **prob >= cutoff && vendor_filter.is_none_or(|v| v == model.vendor))
        .map(|(model, prob)| (model, *prob))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Sort by probability descending for deterministic tie-breaking
    candidates.sort_by(|(left_model, left_prob), (right_model, right_prob)| {
        right_prob
            .partial_cmp(left_prob)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left_model.name.cmp(&right_model.name))
    });

    // Weighted random sampling
    weighted_sample(&candidates)
}

/// Effort-aware variant of [`pick_for_phase`].
///
/// For [`EffortLevel::Tough`], runs the existing weighted-random selection
/// over the tough-eligible subset (Claude-opus and all Codex models). The
/// relative cutoff is computed from the *subset's* `max_prob` so a high-prob
/// sonnet or Kimi cannot raise the bar and exclude every eligible candidate.
/// Falls back to the full unfiltered slice only when the subset selection
/// returns `None`, so the run still launches if no eligible model has quota.
///
/// For [`EffortLevel::Low`] and [`EffortLevel::Normal`], delegates straight to
/// [`pick_for_phase`] so non-tough behavior is byte-identical.
pub fn pick_for_phase_with_effort<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    vendor_filter: Option<VendorKind>,
    version_index: &VersionIndex,
    effort: EffortLevel,
    cheap: bool,
) -> Option<SelectionOutcome<'a>> {
    if cheap {
        let eligible: Vec<CachedModel> = models
            .iter()
            .filter(|m| is_cheap_eligible(m))
            .cloned()
            .collect();

        if !eligible.is_empty()
            && let Some(chosen) = pick_for_phase(&eligible, phase, vendor_filter, version_index)
            && let Some(found) = models
                .iter()
                .find(|m| m.vendor == chosen.vendor && m.name == chosen.name)
        {
            return Some(SelectionOutcome::ok(found));
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

    let eligible: Vec<CachedModel> = models
        .iter()
        .filter(|m| is_tough_eligible(m))
        .cloned()
        .collect();

    if !eligible.is_empty()
        && let Some(chosen) = pick_for_phase(&eligible, phase, vendor_filter, version_index)
    {
        // Map the borrowed pick (over the local `eligible` Vec) back to a
        // reference into the caller's slice so the returned lifetime is `'a`.
        if let Some(found) = models
            .iter()
            .find(|m| m.vendor == chosen.vendor && m.name == chosen.name)
        {
            return Some(SelectionOutcome::ok(found));
        }
    }

    pick_for_phase(models, phase, vendor_filter, version_index).map(SelectionOutcome::ok)
}

/// Select a model for review with unused-vendor preference.
///
/// Prefers models from vendors not yet used, then falls back to any unused
/// model. All weighted by Review phase probability.
pub fn select_for_review<'a>(
    models: &'a [CachedModel],
    used_vendors: &[VendorKind],
    used_models: &[(VendorKind, String)],
    version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    // 1. Different vendor AND different model
    let fresh_vendor: Vec<(&CachedModel, f64)> = models
        .iter()
        .filter(|m| {
            !used_vendors.contains(&m.vendor) && !used_models.contains(&(m.vendor, m.name.clone()))
        })
        .map(|m| {
            (
                m,
                selection_probability(m, SelectionPhase::Review, version_index),
            )
        })
        .collect();

    if let Some(model) = weighted_sample(&fresh_vendor) {
        return Some(model);
    }

    // 2. Same vendor but different model
    let fresh_model: Vec<(&CachedModel, f64)> = models
        .iter()
        .filter(|m| !used_models.contains(&(m.vendor, m.name.clone())))
        .map(|m| {
            (
                m,
                selection_probability(m, SelectionPhase::Review, version_index),
            )
        })
        .collect();

    weighted_sample(&fresh_model)
}

/// Effort-aware variant of [`select_for_review`].
///
/// For [`EffortLevel::Tough`], applies [`is_tough_eligible`] to each
/// diversity tier (fresh-vendor, then fresh-model) before sampling. Only
/// when every tough-eligible model is unavailable in *both* tiers does it
/// degrade to the unfiltered selection — eligibility dominates diversity.
/// In particular, if the only tough-eligible model with quota was already
/// used by the coder, the reviewer reuses it rather than picking a fresh
/// sonnet or Kimi.
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
        let eligible: Vec<&CachedModel> = models.iter().filter(|m| is_cheap_eligible(m)).collect();
        if let Some(model) =
            select_for_review_from_eligible(&eligible, used_vendors, used_models, version_index)
        {
            return Some(SelectionOutcome::ok(model));
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

    let eligible: Vec<&CachedModel> = models.iter().filter(|m| is_tough_eligible(m)).collect();

    select_for_review_from_eligible(&eligible, used_vendors, used_models, version_index)
        .map(SelectionOutcome::ok)
        .or_else(|| {
            // Degraded fallback: no tough-eligible model has any quota at all —
            // run the original diversity logic over the full slice so the review
            // still launches.
            select_for_review(models, used_vendors, used_models, version_index)
                .map(SelectionOutcome::ok)
        })
}

fn select_for_review_from_eligible<'a>(
    eligible: &[&'a CachedModel],
    used_vendors: &[VendorKind],
    used_models: &[(VendorKind, String)],
    version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    // Tier 1: eligible AND fresh-vendor AND fresh-model.
    let fresh_vendor: Vec<(&CachedModel, f64)> = eligible
        .iter()
        .filter(|m| {
            !used_vendors.contains(&m.vendor) && !used_models.contains(&(m.vendor, m.name.clone()))
        })
        .map(|m| {
            (
                *m,
                selection_probability(m, SelectionPhase::Review, version_index),
            )
        })
        .filter(|(_, prob)| *prob > 0.0)
        .collect();
    if let Some(model) = weighted_sample(&fresh_vendor) {
        return Some(model);
    }

    // Tier 2: eligible AND fresh-model (any vendor).
    let fresh_model: Vec<(&CachedModel, f64)> = eligible
        .iter()
        .filter(|m| !used_models.contains(&(m.vendor, m.name.clone())))
        .map(|m| {
            (
                *m,
                selection_probability(m, SelectionPhase::Review, version_index),
            )
        })
        .filter(|(_, prob)| *prob > 0.0)
        .collect();
    if let Some(model) = weighted_sample(&fresh_model) {
        return Some(model);
    }

    // Tier 3: any eligible model, even if used by the coder.
    // This is "eligibility dominates diversity": prefer reusing Claude-opus
    // over a fresh sonnet/Kimi when no fresh eligible model is available.
    let any_eligible: Vec<(&CachedModel, f64)> = eligible
        .iter()
        .map(|m| {
            (
                *m,
                selection_probability(m, SelectionPhase::Review, version_index),
            )
        })
        .filter(|(_, prob)| *prob > 0.0)
        .collect();
    if let Some(model) = weighted_sample(&any_eligible) {
        return Some(model);
    }

    None
}

/// Select a model excluding a list of models, with diversity bonus for non-last-failed vendors.
///
/// `excluded`: Models matching any (vendor, name) pair are excluded.
/// `last_failed_vendor`: If `Some(v)`, models from vendors other than `v` receive
/// a 1.3× diversity bonus before cutoff and sampling.
pub fn select_excluding<'a>(
    models: &'a [CachedModel],
    phase: SelectionPhase,
    excluded: &[(VendorKind, String)],
    last_failed_vendor: Option<VendorKind>,
    version_index: &VersionIndex,
) -> Option<&'a CachedModel> {
    if models.is_empty() {
        return None;
    }

    // Compute probabilities with diversity bonus
    let mut candidates: Vec<(&CachedModel, f64)> = models
        .iter()
        .filter(|m| !excluded.contains(&(m.vendor, m.name.clone())))
        .map(|m| {
            let mut prob = selection_probability(m, phase, version_index);
            if last_failed_vendor.is_some_and(|v| v != m.vendor) {
                prob *= 1.3;
            }
            (m, prob)
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Find max probability (after diversity bonus)
    let max_prob = candidates
        .iter()
        .map(|(_, prob)| *prob)
        .fold(0.0_f64, f64::max);

    if max_prob <= 0.0 {
        return None;
    }

    // Apply relative cutoff
    let cutoff = max_prob * SELECTION_CONFIG.min_selection_probability_ratio;
    candidates.retain(|(_, prob)| *prob >= cutoff);

    if candidates.is_empty() {
        return None;
    }

    // Sort by probability descending for deterministic tie-breaking
    candidates.sort_by(|(left_model, left_prob), (right_model, right_prob)| {
        right_prob
            .partial_cmp(left_prob)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left_model.name.cmp(&right_model.name))
    });

    weighted_sample(&candidates)
}

/// Weighted random sampling from candidates.
/// Returns None if candidates is empty or all weights are zero.
fn weighted_sample<'a>(candidates: &[(&'a CachedModel, f64)]) -> Option<&'a CachedModel> {
    if candidates.is_empty() {
        return None;
    }

    let total: f64 = candidates.iter().map(|(_, weight)| *weight).sum();

    if total <= 0.0 {
        // All weights zero — return lowest-ranked (first after sort)
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

    // Floating-point rounding — return last
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
