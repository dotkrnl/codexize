use super::config::SelectionPhase;
#[cfg(test)]
use super::types::VendorKind;
use super::types::{CachedModel, ScoreSource};
use std::sync::{Mutex, OnceLock};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionEvent {
    ZeroAsMissing { axis: String, phase: String },
}
fn selection_events() -> &'static Mutex<Vec<SelectionEvent>> {
    static EVENTS: OnceLock<Mutex<Vec<SelectionEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}
pub fn selection_events_snapshot() -> Vec<SelectionEvent> {
    // SAFETY: `selection_events()` guards a `Vec<SelectionEvent>` whose
    // only mutators are `push`/`clear` — neither can panic — so the mutex
    // poison branch is only defensive for future mutators.
    selection_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}
#[cfg(test)]
fn clear_selection_events() {
    // SAFETY: see `selection_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    selection_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
}
fn record_zero_as_missing(axis: &str, phase: &str) {
    // SAFETY: see `selection_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    selection_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(SelectionEvent::ZeroAsMissing {
            axis: axis.to_string(),
            phase: phase.to_string(),
        });
}
const ZERO_THRESHOLD: f64 = 1e-9;
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
pub fn candidate_pool_weights(candidates: &[CandidateRef<'_>], phase: SelectionPhase) -> Vec<f64> {
    // Keep output parallel to the input slice; ineligible candidates are "dropped" as zero weights.
    let mut weights = vec![0.0; candidates.len()];
    let mut survivors = Vec::new();
    for (index, model) in candidates.iter().enumerate() {
        let Some(score) = phase_rank_score(model, phase) else {
            continue;
        };
        let Some(effective_quota) = effective_quota_percent(model) else {
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
fn effective_quota_percent(model: &CachedModel) -> Option<u8> {
    match model.quota_percent {
        Some(0) => None,
        Some(quota) => Some(quota),
        None => Some(UNKNOWN_QUOTA_PERCENT),
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
/// Single-model phase score for legacy callers (dashboard fixture diff
/// tooling, mainly). Returns the raw 0–100 ipbr phase score, NOT a
/// normalized probability — callers that need the row's pool share must use
/// [`candidate_pool_weights`] directly. Models with `Some(0)` quota or no
/// ipbr score for the phase return `0.0`.
pub fn phase_score_for_legacy_callers(model: &CachedModel, phase: SelectionPhase) -> f64 {
    if model.quota_percent == Some(0) {
        return 0.0;
    }
    phase_rank_score(model, phase).unwrap_or(0.0)
}
/// Stamp `fallback:overall` provenance on missing or zero-as-missing legacy
/// axes, and emit counter events for zero-as-missing substitutions.
pub fn stamp_selection_provenance(model: &mut CachedModel) {
    let mut seen = std::collections::HashSet::new();
    for phase in SelectionPhase::ALL {
        for &axis in phase.axes() {
            match model.axis(axis) {
                Some(v) if v <= ZERO_THRESHOLD => {
                    if seen.insert(axis) {
                        model
                            .axis_provenance
                            .insert(axis.to_string(), "fallback:overall".to_string());
                    }
                    record_zero_as_missing(axis, phase.name());
                }
                None => {
                    if seen.insert(axis) {
                        model
                            .axis_provenance
                            .insert(axis.to_string(), "fallback:overall".to_string());
                    }
                }
                _ => {
                    seen.insert(axis);
                }
            }
        }
    }
}
#[cfg(test)]
#[path = "ranking_tests.rs"]
mod tests;
