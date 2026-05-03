use super::config::SelectionPhase;
use super::ranking::{VersionIndex, selection_probability};
use super::types::{CachedModel, VendorKind};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// Returns the set of model names that should be visible in the UI.
///
/// Union of top-3-by-probability per phase, plus per-vendor backfill using
/// `current_score` for vendors with no model already in the union.
pub fn visible_models(models: &[CachedModel], version_index: &VersionIndex) -> BTreeSet<String> {
    let mut visible = BTreeSet::new();

    for phase in [
        SelectionPhase::Idea,
        SelectionPhase::Planning,
        SelectionPhase::Build,
        SelectionPhase::Review,
    ] {
        let mut ranked: Vec<(&CachedModel, f64)> = models
            .iter()
            .map(|m| (m, selection_probability(m, phase, version_index)))
            .collect();

        ranked.sort_by(|(a_model, a_prob), (b_model, b_prob)| {
            b_prob
                .partial_cmp(a_prob)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a_model.name.cmp(&b_model.name))
        });

        for (model, _) in ranked.into_iter().take(3) {
            visible.insert(model.name.clone());
        }
    }

    // Per-vendor backfill: for vendors with no model in the union, add the one
    // with the best current_score
    let visible_vendors: BTreeSet<VendorKind> = models
        .iter()
        .filter(|m| visible.contains(&m.name))
        .map(|m| m.vendor)
        .collect();

    for vendor in [
        VendorKind::Claude,
        VendorKind::Codex,
        VendorKind::Gemini,
        VendorKind::Kimi,
    ] {
        if visible_vendors.contains(&vendor) {
            continue;
        }
        if let Some(best) = models.iter().filter(|m| m.vendor == vendor).max_by(|a, b| {
            a.current_score
                .partial_cmp(&b.current_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        }) {
            visible.insert(best.name.clone());
        }
    }

    visible
}

/// Computes dense ranks for the given phase, ordered by probability descending.
///
/// Returns a map from model name to 1-based rank. Models with equal probability
/// receive the same rank; the next distinct probability gets rank = prev + count.
pub fn phase_rank(
    models: &[CachedModel],
    phase: SelectionPhase,
    version_index: &VersionIndex,
) -> BTreeMap<String, u32> {
    let mut ranked: Vec<(&CachedModel, f64)> = models
        .iter()
        .map(|m| (m, selection_probability(m, phase, version_index)))
        .collect();

    ranked.sort_by(|(a_model, a_prob), (b_model, b_prob)| {
        b_prob
            .partial_cmp(a_prob)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a_model.name.cmp(&b_model.name))
    });

    let mut result = BTreeMap::new();
    let mut current_rank: u32 = 0;
    let mut prev_prob: Option<f64> = None;

    for (model, prob) in &ranked {
        if prev_prob.is_none_or(|p: f64| (*prob - p).abs() > f64::EPSILON) {
            current_rank += 1;
        }
        result.insert(model.name.clone(), current_rank);
        prev_prob = Some(*prob);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::super::ranking::build_version_index;
    use super::*;

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
                ("complexity".to_string(), 0.85),
                ("edgecases".to_string(), 0.85),
                ("contextawareness".to_string(), 0.85),
                ("taskcompletion".to_string(), 0.85),
                ("stability".to_string(), 0.85),
            ],
            axis_provenance: std::collections::BTreeMap::new(),
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                idea: Some(85.0),
                planning: Some(85.0),
                build: Some(85.0),
                review: Some(85.0),
            },
            score_source: crate::selection::ScoreSource::Ipbr,
            ipbr_row_matched: true,
            quota_percent: Some(quota),
            quota_resets_at: None,
            display_order: 0,
            fallback_from: None,
        }
    }

    #[test]
    fn visible_models_includes_top_3_per_phase() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-a", 80),
            sample_model(VendorKind::Claude, "claude-b", 80),
            sample_model(VendorKind::Claude, "claude-c", 80),
            CachedModel {
                quota_percent: Some(1),
                ..sample_model(VendorKind::Claude, "claude-d", 1)
            },
        ];
        let index = build_version_index(&models);

        let visible = visible_models(&models, &index);

        // Top 3 by probability should be included (claude-d has very low quota → low prob)
        assert!(visible.contains("claude-a"));
        assert!(visible.contains("claude-b"));
        assert!(visible.contains("claude-c"));
    }

    #[test]
    fn visible_models_backfills_vendor_with_no_top_model() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-top", 90),
            sample_model(VendorKind::Codex, "codex-top", 90),
            sample_model(VendorKind::Gemini, "gemini-top", 90),
            CachedModel {
                current_score: 70.0,
                quota_percent: Some(1),
                ..sample_model(VendorKind::Kimi, "kimi-latest", 1)
            },
        ];
        let index = build_version_index(&models);

        let visible = visible_models(&models, &index);

        // Kimi has low probability so won't be top-3 in any phase,
        // but should be backfilled by current_score
        assert!(visible.contains("kimi-latest"));
    }

    #[test]
    fn visible_models_backfill_uses_current_score() {
        let models = vec![
            sample_model(VendorKind::Claude, "claude-top", 90),
            sample_model(VendorKind::Codex, "codex-top", 90),
            sample_model(VendorKind::Gemini, "gemini-top", 90),
            CachedModel {
                current_score: 80.0,
                quota_percent: Some(1),
                ..sample_model(VendorKind::Kimi, "kimi-better", 1)
            },
            CachedModel {
                current_score: 60.0,
                quota_percent: Some(1),
                ..sample_model(VendorKind::Kimi, "kimi-worse", 1)
            },
        ];
        let index = build_version_index(&models);

        let visible = visible_models(&models, &index);

        // kimi-better has higher current_score so it should be the backfill pick
        assert!(visible.contains("kimi-better"));
        assert!(!visible.contains("kimi-worse"));
    }

    #[test]
    fn phase_rank_assigns_dense_ranks() {
        let models = vec![
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(95.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..sample_model(VendorKind::Claude, "top", 80)
            },
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(50.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..sample_model(VendorKind::Codex, "mid", 80)
            },
            CachedModel {
                quota_percent: Some(0),
                ..sample_model(VendorKind::Gemini, "low", 80)
            },
        ];
        let index = build_version_index(&models);

        let ranks = phase_rank(&models, SelectionPhase::Build, &index);

        assert_eq!(ranks.len(), 3);
        let top_rank = ranks["top"];
        let mid_rank = ranks["mid"];
        let low_rank = ranks["low"];
        assert!(top_rank < mid_rank);
        assert!(mid_rank < low_rank);
    }

    #[test]
    fn phase_rank_ties_get_same_rank() {
        let models = vec![
            sample_model(VendorKind::Claude, "a", 80),
            sample_model(VendorKind::Codex, "b", 80),
        ];
        let index = build_version_index(&models);

        let ranks = phase_rank(&models, SelectionPhase::Build, &index);

        // Same quota, same axes, same overall_score → same probability → same rank
        assert_eq!(ranks["a"], ranks["b"]);
    }

    #[test]
    fn phase_rank_dense_after_tie() {
        // Two tied top models followed by a strictly lower-probability model
        // should produce dense ranks 1, 1, 2 — not the competition-rank 1, 1, 3.
        let models = vec![
            sample_model(VendorKind::Claude, "tie-a", 80),
            sample_model(VendorKind::Codex, "tie-b", 80),
            CachedModel {
                quota_percent: Some(0),
                ..sample_model(VendorKind::Gemini, "low", 80)
            },
        ];
        let index = build_version_index(&models);

        let ranks = phase_rank(&models, SelectionPhase::Build, &index);

        assert_eq!(ranks["tie-a"], 1);
        assert_eq!(ranks["tie-b"], 1);
        assert_eq!(ranks["low"], 2);
    }

    #[test]
    fn phase_rank_empty_models() {
        let index = build_version_index(&[]);

        let ranks = phase_rank(&[], SelectionPhase::Build, &index);

        assert!(ranks.is_empty());
    }

    #[test]
    fn visible_models_empty_input() {
        let index = build_version_index(&[]);

        let visible = visible_models(&[], &index);

        assert!(visible.is_empty());
    }

    #[test]
    fn version_index_built_post_collapse() {
        use super::super::ranking::build_version_index;

        // After Kimi collapse, the version index should only contain "kimi-latest"
        let models = vec![
            sample_model(VendorKind::Claude, "claude-sonnet-4-6", 80),
            CachedModel {
                name: "kimi-latest".to_string(),
                ..sample_model(VendorKind::Kimi, "kimi-latest", 80)
            },
        ];
        let index = build_version_index(&models);

        // kimi-latest is the only Kimi model → rank 0 (no penalty)
        assert_eq!(index.version_rank(VendorKind::Kimi, "kimi-latest"), 0);
    }
}
