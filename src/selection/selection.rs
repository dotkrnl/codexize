use super::config::{SELECTION_CONFIG, SelectionPhase};
use super::ranking::{selection_probability, VersionIndex};
use super::types::{CachedModel, VendorKind};
use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

#[cfg(test)]
static TEST_SAMPLE_SEED: AtomicU64 = AtomicU64::new(0);

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
    let max_prob = probabilities
        .iter()
        .copied()
        .fold(0.0_f64, f64::max);

    if max_prob <= 0.0 {
        return None;
    }

    // Apply relative cutoff
    let cutoff = max_prob * SELECTION_CONFIG.min_selection_probability_ratio;

    // Build candidate list: apply cutoff and vendor filter
    let mut candidates: Vec<(&CachedModel, f64)> = models
        .iter()
        .zip(probabilities.iter())
        .filter(|(model, prob)| {
            **prob >= cutoff && vendor_filter.is_none_or(|v| v == model.vendor)
        })
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
            !used_vendors.contains(&m.vendor)
                && !used_models.contains(&(m.vendor, m.name.clone()))
        })
        .map(|m| (m, selection_probability(m, SelectionPhase::Review, version_index)))
        .collect();

    if let Some(model) = weighted_sample(&fresh_vendor) {
        return Some(model);
    }

    // 2. Same vendor but different model
    let fresh_model: Vec<(&CachedModel, f64)> = models
        .iter()
        .filter(|m| !used_models.contains(&(m.vendor, m.name.clone())))
        .map(|m| (m, selection_probability(m, SelectionPhase::Review, version_index)))
        .collect();

    weighted_sample(&fresh_model)
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
mod tests {
    use super::*;
    use super::super::ranking::build_version_index;

    fn sample_model(vendor: VendorKind, name: &str, quota: u8) -> CachedModel {
        CachedModel {
            vendor,
            name: name.to_string(),
            overall_score: 85.0,
            current_score: 85.0,
            standard_error: 2.0,
            axes: vec![
                ("codequality".to_string(), 0.85),
                ("correctness".to_string(), 0.85),
                ("debugging".to_string(), 0.85),
                ("safety".to_string(), 0.85),
            ],
            quota_percent: Some(quota),
            display_order: 0,
            fallback_from: None,
        }
    }

    #[test]
    fn pick_for_phase_returns_none_for_empty() {
        let index = build_version_index(&[]);
        let result = pick_for_phase(&[], SelectionPhase::Build, None, &index);
        assert!(result.is_none());
    }

    #[test]
    fn pick_for_phase_applies_relative_cutoff() {
        let models = vec![
            sample_model(VendorKind::Claude, "high", 80),
            sample_model(VendorKind::Codex, "low", 1), // Very low quota
        ];
        let index = build_version_index(&models);

        // With cutoff ratio 1/3 and quota=1, the low-quota model should be excluded
        // quota_weight(1) ≈ 0.0016, quota_weight(80) = 1.0, ratio < 1/3
        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let chosen = pick_for_phase(&models, SelectionPhase::Build, None, &index)
            .expect("should pick high-quota model");
        assert_eq!(chosen.name, "high");
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn pick_for_phase_respects_vendor_filter() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-model", 80),
            sample_model(VendorKind::Codex, "codex-model", 80),
        ];
        let index = build_version_index(&models);

        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let chosen = pick_for_phase(&models, SelectionPhase::Build, Some(VendorKind::Claude), &index)
            .expect("should pick claude");
        assert_eq!(chosen.vendor, VendorKind::Claude);
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn pick_for_phase_uses_unfiltered_max_for_cutoff() {
        // High-prob Claude model sets the cutoff bar, even when filtering to Codex
        let models = vec![
            sample_model(VendorKind::Claude, "high-claude", 90),
            sample_model(VendorKind::Codex, "medium-codex", 50),
            sample_model(VendorKind::Codex, "low-codex", 10),
        ];
        let index = build_version_index(&models);

        // Filter to Codex only — cutoff still based on Claude's high prob
        let chosen = pick_for_phase(&models, SelectionPhase::Build, Some(VendorKind::Codex), &index);

        // medium-codex should survive cutoff; low-codex should not
        assert!(chosen.is_some());
        assert_eq!(chosen.unwrap().vendor, VendorKind::Codex);
    }

    #[test]
    fn select_for_review_prefers_fresh_vendor() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-1", 80),
            sample_model(VendorKind::Codex, "codex-1", 80),
        ];
        let index = build_version_index(&models);

        let used_vendors = vec![VendorKind::Claude];
        let used_models = vec![(VendorKind::Claude, "claude-1".to_string())];

        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let chosen = select_for_review(&models, &used_vendors, &used_models, &index)
            .expect("should pick fresh vendor");
        assert_eq!(chosen.vendor, VendorKind::Codex);
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn select_for_review_falls_back_to_unused_model_same_vendor() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-1", 80),
            sample_model(VendorKind::Claude, "claude-2", 80),
        ];
        let index = build_version_index(&models);

        let used_vendors = vec![VendorKind::Claude];
        let used_models = vec![(VendorKind::Claude, "claude-1".to_string())];

        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let chosen = select_for_review(&models, &used_vendors, &used_models, &index)
            .expect("should pick unused model");
        assert_eq!(chosen.name, "claude-2");
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn select_for_review_returns_none_when_all_used() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-1", 80),
        ];
        let index = build_version_index(&models);

        let used_vendors = vec![VendorKind::Claude];
        let used_models = vec![(VendorKind::Claude, "claude-1".to_string())];

        let chosen = select_for_review(&models, &used_vendors, &used_models, &index);
        assert!(chosen.is_none());
    }

    #[test]
    fn select_excluding_excludes_listed_models() {
        let models = vec![
            sample_model(VendorKind::Claude, "excluded", 80),
            sample_model(VendorKind::Codex, "included", 80),
        ];
        let index = build_version_index(&models);

        let excluded = vec![(VendorKind::Claude, "excluded".to_string())];

        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let chosen = select_excluding(&models, SelectionPhase::Build, &excluded, None, &index)
            .expect("should pick non-excluded");
        assert_eq!(chosen.name, "included");
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn select_excluding_applies_diversity_bonus() {
        let models = vec![
            sample_model(VendorKind::Claude, "same-vendor", 80),
            sample_model(VendorKind::Codex, "other-vendor", 80),
        ];
        let index = build_version_index(&models);

        // Both have same quota, but Codex gets 1.3× diversity bonus
        // With bonus, Codex has 1.3× the probability, so should win most samples
        let mut codex_count = 0;
        for seed in 1000..1100_u64 {
            TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
            if let Some(chosen) = select_excluding(
                &models,
                SelectionPhase::Build,
                &[],
                Some(VendorKind::Claude),
                &index,
            ) {
                if chosen.vendor == VendorKind::Codex {
                    codex_count += 1;
                }
            }
        }

        // Codex should win at least 50% of the time (actual ratio should be ~1.3:1 or 56.5%)
        assert!(codex_count > 50, "Codex won {} out of 100", codex_count);
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn select_excluding_returns_none_when_all_excluded() {
        let models = vec![
            sample_model(VendorKind::Claude, "model-1", 80),
        ];
        let index = build_version_index(&models);

        let excluded = vec![(VendorKind::Claude, "model-1".to_string())];

        let chosen = select_excluding(&models, SelectionPhase::Build, &excluded, None, &index);
        assert!(chosen.is_none());
    }

    #[test]
    fn weighted_sample_returns_none_for_empty() {
        let candidates: Vec<(&CachedModel, f64)> = vec![];
        assert!(weighted_sample(&candidates).is_none());
    }

    #[test]
    fn weighted_sample_returns_first_when_all_zero_weight() {
        let m1 = sample_model(VendorKind::Claude, "first", 80);
        let m2 = sample_model(VendorKind::Codex, "second", 80);
        let candidates = vec![(&m1, 0.0), (&m2, 0.0)];

        let chosen = weighted_sample(&candidates).expect("should pick first");
        assert_eq!(chosen.name, "first");
    }

    #[test]
    fn weighted_sample_uses_weights_for_random() {
        let m1 = sample_model(VendorKind::Claude, "high-weight", 80);
        let m2 = sample_model(VendorKind::Codex, "low-weight", 80);
        let candidates = vec![(&m1, 1000.0), (&m2, 1.0)];

        // High weight should almost always win
        let mut high_count = 0;
        for seed in 1..100_u64 {
            TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
            let chosen = weighted_sample(&candidates).expect("should pick");
            if chosen.name == "high-weight" {
                high_count += 1;
            }
        }

        assert!(high_count > 90); // Should win most of the time
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }
}
