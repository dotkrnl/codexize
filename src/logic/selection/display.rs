use super::config::SelectionPhase;
use super::ranking::phase_rank_score;
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
pub fn visible_models(models: &[CachedModel]) -> BTreeSet<String> {
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
pub fn phase_rank(models: &[CachedModel], phase: SelectionPhase) -> BTreeMap<String, u32> {
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
#[path = "display_tests.rs"]
mod tests;
