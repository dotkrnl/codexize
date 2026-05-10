use super::config::SelectionPhase;
#[cfg(test)]
use super::types::SubscriptionKind;
use super::types::{CachedModel, ScoreSource};
const RANK_SOFTMAX_TEMPERATURE: f64 = 5.5;
const UNKNOWN_QUOTA_PERCENT: u8 = 30;
pub type CandidateRef<'a> = &'a CachedModel;
/// Return the authoritative ipbr rank score for `phase`, if this model has one.
pub fn phase_rank_score(model: &CachedModel, phase: SelectionPhase) -> Option<f64> {
    if model.score_source != ScoreSource::Ipbr {
        return None;
    }
    match phase {
        SelectionPhase::Idea => model.ipbr_phase_scores.idea,
        SelectionPhase::Planning => model.ipbr_phase_scores.planning,
        SelectionPhase::Build => model.ipbr_phase_scores.build,
        SelectionPhase::Review => model.ipbr_phase_scores.review,
    }
}
/// Return pool-scoped sampling weights in the same order as `candidates`.
///
/// Per spec §"Selection algorithm" (operator decision, 2026-05-09): the
/// sampler weights combine the dashboard ipbr phase score with the row's
/// *max `effective_quota_for_tiebreak` across enabled providers*. Free /
/// quota_disabled providers therefore push the row to 100% headroom in the
/// sampler even when no fetched quota exists, while a row with only fetched
/// providers carries their actual remaining quota.
pub fn candidate_pool_weights(candidates: &[CandidateRef<'_>], phase: SelectionPhase) -> Vec<f64> {
    // Keep output parallel to the input slice; ineligible candidates are "dropped" as zero weights.
    let mut weights = vec![0.0; candidates.len()];
    let mut survivors = Vec::new();
    for (index, model) in candidates.iter().enumerate() {
        let Some(score) = phase_rank_score(model, phase) else {
            continue;
        };
        let Some(effective_quota) = effective_row_quota(model) else {
            continue;
        };
        survivors.push((index, score, effective_quota));
    }
    if survivors.is_empty() {
        return weights;
    }
    let max_score = survivors
        .iter()
        .map(|(_, score, _)| *score)
        .fold(f64::NEG_INFINITY, f64::max);
    let exp_weights: Vec<f64> = survivors
        .iter()
        .map(|(_, score, _)| ((score - max_score) / RANK_SOFTMAX_TEMPERATURE).exp())
        .collect();
    let exp_total: f64 = exp_weights.iter().sum();
    let pool_best_quota = survivors
        .iter()
        .map(|(_, _, quota)| *quota)
        .max()
        .unwrap_or(UNKNOWN_QUOTA_PERCENT);
    for ((index, _, quota), exp_weight) in survivors.iter().zip(exp_weights.iter()) {
        let rank_weight = exp_weight / exp_total;
        let quota_factor = relative_quota_factor(pool_best_quota - *quota);
        weights[*index] = rank_weight * quota_factor;
    }
    weights
}
/// Row-level effective quota that drives the sampler. Returns:
/// - `None` when the row is explicitly exhausted (every enabled
///   provider's `effective_quota_for_tiebreak` is `0`, *and* no
///   provider is unknown), so the sampler drops the row;
/// - `Some(value)` otherwise, where `value` is the spec's
///   "max over enabled providers of `effective_quota_for_tiebreak`,
///   unknown → 0". Free and `quota_disabled` providers pin this to 100;
///   subscriptions that failed their quota fetch contribute 50.
pub fn effective_row_quota(model: &CachedModel) -> Option<u8> {
    if model.candidates.is_empty() {
        return None;
    }
    let mut max_quota: Option<u8> = None;
    let mut has_unknown = false;
    for candidate in model.candidates.iter().filter(|c| c.enabled) {
        match candidate.effective_quota() {
            Some(value) => {
                max_quota = Some(max_quota.map_or(value, |best| best.max(value)));
            }
            None => has_unknown = true,
        }
    }
    match (max_quota, has_unknown) {
        (Some(0), false) => None,
        (Some(value), _) => Some(value),
        (None, true) => Some(UNKNOWN_QUOTA_PERCENT),
        (None, false) => None,
    }
}
fn relative_quota_factor(deficit: u8) -> f64 {
    if deficit <= 20 {
        return 1.0;
    }
    if deficit >= 40 {
        return 0.10;
    }
    let t = ((f64::from(deficit) - 20.0) / 20.0).clamp(0.0, 1.0);
    let smooth = t * t * (3.0 - 2.0 * t);
    1.0 - 0.90 * smooth
}
#[cfg(test)]
#[path = "ranking_tests.rs"]
mod tests;
