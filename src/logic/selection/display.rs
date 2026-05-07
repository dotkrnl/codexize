use super::config::SelectionPhase;
use super::ranking::{candidate_pool_weights, phase_rank_score};
use super::types::{CachedModel, VendorKind};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
/// Pool weight a model must exceed in some phase to count as "selectable
/// often enough to deserve a row." Strict `>`: a model that ties exactly
/// 10% of its pool falls back on the per-vendor floor instead.
const VISIBILITY_WEIGHT_THRESHOLD: f64 = 0.10;
/// Global model ordering used wherever a "top-down" list of models is
/// rendered or a vendor representative is picked: ipbr Build phase rank
/// (descending), then vendor, then name. Models without an ipbr Build score
/// sort to the bottom — that way an unscored row can never be lifted above
/// an ipbr-ranked peer by cosmetic summary fields.
pub fn build_rank_order(a: &CachedModel, b: &CachedModel) -> Ordering {
    let a_score = phase_rank_score(a, SelectionPhase::Build);
    let b_score = phase_rank_score(b, SelectionPhase::Build);
    match (a_score, b_score) {
        (Some(sa), Some(sb)) => sb.partial_cmp(&sa).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| a.vendor.cmp(&b.vendor))
    .then_with(|| a.name.cmp(&b.name))
}
/// Returns the set of model names that should be visible in the UI.
///
/// A model is visible when its candidate-pool weight exceeds
/// `VISIBILITY_WEIGHT_THRESHOLD` (10%) in *any* of the four phases; that
/// keeps "could plausibly be sampled here" rows on the table and drops
/// rows squeezed below the threshold by softmax + quota factors. The
/// per-vendor floor still admits one row per vendor (chosen by the global
/// `build_rank_order`) so a vendor can never disappear when its scores
/// stay below the threshold. Cosmetic `current_score` / `overall_score`
/// MUST NOT influence visibility.
pub fn visible_models(models: &[CachedModel]) -> BTreeSet<String> {
    let mut visible = BTreeSet::new();
    let candidates: Vec<&CachedModel> = models.iter().collect();
    for phase in [
        SelectionPhase::Idea,
        SelectionPhase::Planning,
        SelectionPhase::Build,
        SelectionPhase::Review,
    ] {
        let weights = candidate_pool_weights(&candidates, phase);
        for (model, weight) in candidates.iter().zip(weights.iter()) {
            if *weight > VISIBILITY_WEIGHT_THRESHOLD {
                visible.insert(model.name.clone());
            }
        }
    }
    // Per-vendor floor: keep at least one row per vendor visible even when
    // no phase pool lifted any of its rows above the threshold. The pick
    // uses `build_rank_order` so the vendor's strongest ipbr-scored row
    // wins, falling back to alphabetical when no ipbr score exists.
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
        VendorKind::Opencode,
    ] {
        if visible_vendors.contains(&vendor) {
            continue;
        }
        if let Some(best) = models
            .iter()
            .filter(|m| m.vendor == vendor)
            .min_by(|a, b| build_rank_order(a, b))
        {
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
