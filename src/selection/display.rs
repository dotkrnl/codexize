use super::config::SelectionPhase;
use super::ranking::{VersionIndex, phase_rank_score};
use super::types::{CachedModel, VendorKind};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// Returns the set of model names that should be visible in the UI.
///
/// Union of top-3-by-pure-ipbr-phase-rank across the four phases, plus
/// per-vendor backfill (by inventory `display_order`) for vendors that
/// would otherwise have no representative in the union. Cosmetic
/// `current_score` / `overall_score` MUST NOT influence visibility, since
/// the spec restricts those fields to display-only roles.
pub fn visible_models(models: &[CachedModel], _version_index: &VersionIndex) -> BTreeSet<String> {
    let mut visible = BTreeSet::new();

    for phase in [
        SelectionPhase::Idea,
        SelectionPhase::Planning,
        SelectionPhase::Build,
        SelectionPhase::Review,
    ] {
        // Only models with an authoritative ipbr phase score compete for
        // top-3 in this phase. Inventory-only models without a score for
        // the phase are intentionally absent here; vendor backfill below
        // still keeps each vendor visible.
        let mut ranked: Vec<(&CachedModel, f64)> = models
            .iter()
            .filter_map(|m| phase_rank_score(m, phase).map(|score| (m, score)))
            .collect();

        ranked.sort_by(|(a_model, a_score), (b_model, b_score)| {
            b_score
                .partial_cmp(a_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a_model.name.cmp(&b_model.name))
        });

        for (model, _) in ranked.into_iter().take(3) {
            visible.insert(model.name.clone());
        }
    }

    // Per-vendor backfill: keep at least one model per vendor visible even
    // when no phase rank lifted it into the top-3. The backfill uses
    // inventory ordering (`display_order`, then name) so cosmetic summary
    // scores cannot influence the visible set or substitute for ipbr rank.
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
        if let Some(best) = models.iter().filter(|m| m.vendor == vendor).min_by(|a, b| {
            a.display_order
                .cmp(&b.display_order)
                .then_with(|| a.name.cmp(&b.name))
        }) {
            visible.insert(best.name.clone());
        }
    }

    visible
}

/// Computes dense phase ranks from pure ipbr phase scores.
///
/// Returns a map from model name to 1-based rank, ordered by phase score
/// descending. Models without an ipbr phase score for `phase` are absent
/// from the map (rendered as unscored/unranked by callers). Equal scores
/// get the same rank; the next strictly lower score gets the next rank
/// (dense ranking, e.g. 1, 1, 2 — not competition ranking 1, 1, 3).
pub fn phase_rank(
    models: &[CachedModel],
    phase: SelectionPhase,
    _version_index: &VersionIndex,
) -> BTreeMap<String, u32> {
    let mut ranked: Vec<(&CachedModel, f64)> = models
        .iter()
        .filter_map(|m| phase_rank_score(m, phase).map(|score| (m, score)))
        .collect();

    ranked.sort_by(|(a_model, a_score), (b_model, b_score)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a_model.name.cmp(&b_model.name))
    });

    let mut result = BTreeMap::new();
    let mut current_rank: u32 = 0;
    let mut prev_score: Option<f64> = None;

    for (model, score) in &ranked {
        if prev_score.is_none_or(|p: f64| (*score - p).abs() > f64::EPSILON) {
            current_rank += 1;
        }
        result.insert(model.name.clone(), current_rank);
        prev_score = Some(*score);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::super::ranking::build_version_index;
    use super::*;

    fn ipbr_model(vendor: VendorKind, name: &str, score: f64, quota: Option<u8>) -> CachedModel {
        CachedModel {
            vendor,
            name: name.to_string(),
            overall_score: 85.0,
            current_score: 85.0,
            standard_error: 2.0,
            axes: Vec::new(),
            axis_provenance: std::collections::BTreeMap::new(),
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                idea: Some(score),
                planning: Some(score),
                build: Some(score),
                review: Some(score),
            },
            score_source: crate::selection::ScoreSource::Ipbr,
            ipbr_row_matched: true,
            quota_percent: quota,
            quota_resets_at: None,
            display_order: 0,
            fallback_from: None,
        }
    }

    fn unscored_model(vendor: VendorKind, name: &str, display_order: usize) -> CachedModel {
        CachedModel {
            vendor,
            name: name.to_string(),
            overall_score: 85.0,
            current_score: 85.0,
            standard_error: 2.0,
            axes: Vec::new(),
            axis_provenance: std::collections::BTreeMap::new(),
            ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
            score_source: crate::selection::ScoreSource::None,
            ipbr_row_matched: false,
            quota_percent: Some(80),
            quota_resets_at: None,
            display_order,
            fallback_from: None,
        }
    }

    #[test]
    fn visible_models_includes_top_3_per_phase_by_ipbr_score() {
        let models = vec![
            ipbr_model(VendorKind::Claude, "claude-a", 95.0, Some(80)),
            ipbr_model(VendorKind::Claude, "claude-b", 90.0, Some(80)),
            ipbr_model(VendorKind::Claude, "claude-c", 85.0, Some(80)),
            // Lowest-scored Claude model — outside the top-3 union for any
            // phase, even though quota is healthy.
            ipbr_model(VendorKind::Claude, "claude-d", 10.0, Some(100)),
        ];
        let index = build_version_index(&models);

        let visible = visible_models(&models, &index);

        assert!(visible.contains("claude-a"));
        assert!(visible.contains("claude-b"));
        assert!(visible.contains("claude-c"));
        assert!(
            !visible.contains("claude-d"),
            "lowest-scored model should not enter top-3 across any phase"
        );
    }

    #[test]
    fn visible_models_backfills_missing_vendors_via_display_order() {
        let models = vec![
            ipbr_model(VendorKind::Claude, "claude-top", 95.0, Some(80)),
            ipbr_model(VendorKind::Codex, "codex-top", 95.0, Some(80)),
            ipbr_model(VendorKind::Gemini, "gemini-top", 95.0, Some(80)),
            // Two unscored Kimi models: backfill must pick the one with the
            // lower `display_order`, ignoring cosmetic `current_score`.
            CachedModel {
                current_score: 60.0,
                display_order: 0,
                ..unscored_model(VendorKind::Kimi, "kimi-first", 0)
            },
            CachedModel {
                current_score: 99.0,
                display_order: 5,
                ..unscored_model(VendorKind::Kimi, "kimi-later", 5)
            },
        ];
        let index = build_version_index(&models);

        let visible = visible_models(&models, &index);

        assert!(
            visible.contains("kimi-first"),
            "backfill should follow inventory display_order"
        );
        assert!(
            !visible.contains("kimi-later"),
            "cosmetic current_score must not promote a later inventory entry"
        );
    }

    #[test]
    fn visible_models_inventory_only_model_remains_via_vendor_backfill() {
        // Spec: inventory/CLI-visible models stay visible even with no ipbr
        // score. The backfill rule is the visibility safety net.
        let models = vec![
            ipbr_model(VendorKind::Claude, "claude-top", 95.0, Some(80)),
            ipbr_model(VendorKind::Codex, "codex-top", 95.0, Some(80)),
            ipbr_model(VendorKind::Gemini, "gemini-top", 95.0, Some(80)),
            unscored_model(VendorKind::Kimi, "kimi-cli-only", 0),
        ];
        let index = build_version_index(&models);

        let visible = visible_models(&models, &index);

        assert!(visible.contains("kimi-cli-only"));
    }

    #[test]
    fn phase_rank_orders_by_ipbr_phase_score_descending() {
        let models = vec![
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(95.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..ipbr_model(VendorKind::Claude, "top", 95.0, Some(80))
            },
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(50.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..ipbr_model(VendorKind::Codex, "mid", 50.0, Some(80))
            },
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(10.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..ipbr_model(VendorKind::Gemini, "low", 10.0, Some(80))
            },
        ];
        let index = build_version_index(&models);

        let ranks = phase_rank(&models, SelectionPhase::Build, &index);

        assert_eq!(ranks.len(), 3);
        assert_eq!(ranks["top"], 1);
        assert_eq!(ranks["mid"], 2);
        assert_eq!(ranks["low"], 3);
    }

    #[test]
    fn phase_rank_omits_unscored_and_non_ipbr_models() {
        // Unscored / cosmetic-only models render as unranked: they must
        // not appear in the rank map at all (callers treat absence as
        // "no rank for this phase").
        let mut cosmetic_only = unscored_model(VendorKind::Claude, "cosmetic", 0);
        cosmetic_only.score_source = crate::selection::ScoreSource::Aistupidlevel;
        cosmetic_only.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
            build: Some(99.0),
            ..crate::selection::IpbrPhaseScores::default()
        };

        let models = vec![
            ipbr_model(VendorKind::Codex, "ranked", 80.0, Some(80)),
            unscored_model(VendorKind::Gemini, "inventory-only", 0),
            cosmetic_only,
        ];
        let index = build_version_index(&models);

        let ranks = phase_rank(&models, SelectionPhase::Build, &index);

        assert_eq!(ranks.len(), 1);
        assert_eq!(ranks["ranked"], 1);
        assert!(!ranks.contains_key("inventory-only"));
        assert!(!ranks.contains_key("cosmetic"));
    }

    #[test]
    fn phase_rank_dense_after_tie() {
        let models = vec![
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(90.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..ipbr_model(VendorKind::Claude, "tie-a", 90.0, Some(80))
            },
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(90.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..ipbr_model(VendorKind::Codex, "tie-b", 90.0, Some(80))
            },
            CachedModel {
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    build: Some(50.0),
                    ..crate::selection::IpbrPhaseScores::default()
                },
                ..ipbr_model(VendorKind::Gemini, "lower", 50.0, Some(80))
            },
        ];
        let index = build_version_index(&models);

        let ranks = phase_rank(&models, SelectionPhase::Build, &index);

        assert_eq!(ranks["tie-a"], 1);
        assert_eq!(ranks["tie-b"], 1);
        assert_eq!(ranks["lower"], 2);
    }

    #[test]
    fn phase_rank_empty_when_no_models_or_no_scores() {
        let index = build_version_index(&[]);
        assert!(phase_rank(&[], SelectionPhase::Build, &index).is_empty());

        let unscored = vec![unscored_model(VendorKind::Claude, "a", 0)];
        let index = build_version_index(&unscored);
        assert!(phase_rank(&unscored, SelectionPhase::Build, &index).is_empty());
    }

    #[test]
    fn visible_models_empty_input() {
        let index = build_version_index(&[]);
        assert!(visible_models(&[], &index).is_empty());
    }
}
