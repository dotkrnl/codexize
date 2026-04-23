use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use crate::dashboard;
use super::types::Candidate;

const MIN_ROLE_SCORE_WEIGHT: f64 = 0.05;
const HIGH_VARIANCE_STANDARD_ERROR: f64 = 5.0;
const HIGH_VARIANCE_EXTRA_PENALTY: f64 = 30.0;
const STANDARD_ERROR_PENALTY_MULTIPLIER: f64 = 2.0;

pub const IDEA_AXES: &[&str] = &["complexity", "edgecases", "contextawareness", "taskcompletion"];
pub const PLAN_AXES: &[&str] = &["correctness", "complexity", "edgecases", "stability"];
pub const BUILD_AXES: &[&str] = &["codequality", "correctness", "debugging", "safety"];
pub const REVIEW_AXES: &[&str] = &["correctness", "debugging", "edgecases", "safety", "stability"];

pub fn top_model_union(candidates: &[Candidate]) -> BTreeSet<String> {
    let mut retained = BTreeSet::new();

    // First, ensure at least one model per vendor
    for vendor in [
        super::types::VendorKind::Claude,
        super::types::VendorKind::Codex,
        super::types::VendorKind::Gemini,
        super::types::VendorKind::Kimi,
    ] {
        // Find the best model for this vendor by overall score
        if let Some(best) = candidates
            .iter()
            .filter(|c| c.vendor == vendor)
            .max_by(|a, b| {
                a.overall_score
                    .partial_cmp(&b.overall_score)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| b.display_order.cmp(&a.display_order))
            })
        {
            retained.insert(best.name.clone());
        }
    }

    // Then add top-3 models for each task type
    for selector in [
        |candidate: &Candidate| candidate.idea_probability,
        |candidate: &Candidate| candidate.planning_probability,
        |candidate: &Candidate| candidate.build_probability,
        |candidate: &Candidate| candidate.review_probability,
    ] {
        let mut ranked = candidates.iter().collect::<Vec<_>>();
        ranked.sort_by(|left, right| compare_probability(*left, *right, selector));
        for candidate in ranked.into_iter().take(3) {
            retained.insert(candidate.name.clone());
        }
    }

    retained
}

pub fn rank_map(
    candidates: &[Candidate],
    selector: impl Fn(&Candidate) -> f64 + Copy,
) -> BTreeMap<String, u8> {
    let mut ranked = candidates.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| compare_probability(*left, *right, selector));
    ranked
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.name.clone(), (index + 1).min(99) as u8))
        .collect()
}

fn compare_probability(
    left: &Candidate,
    right: &Candidate,
    selector: impl Fn(&Candidate) -> f64,
) -> Ordering {
    selector(right)
        .partial_cmp(&selector(left))
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            right
                .overall_score
                .partial_cmp(&left.overall_score)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| left.display_order.cmp(&right.display_order))
        .then_with(|| left.name.cmp(&right.name))
}

pub fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .overall_score
        .partial_cmp(&left.overall_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.display_order.cmp(&right.display_order))
        .then_with(|| left.name.cmp(&right.name))
}

pub fn selection_probability(
    model: &dashboard::DashboardModel,
    quota_percent: Option<u8>,
    axes: &[&str],
) -> f64 {
    // Assume 50% when quota is not available so unprobed models still participate
    let quota_weight = quota_percent.unwrap_or(50) as f64 / 100.0;
    if quota_weight <= 0.0 {
        return 0.0;
    }
    let role_score = role_score(model, axes) / 100.0;
    quota_weight * role_score.max(MIN_ROLE_SCORE_WEIGHT).powi(2)
}

fn role_score(model: &dashboard::DashboardModel, axes: &[&str]) -> f64 {
    let axis_map = model.axes.iter().cloned().collect::<std::collections::BTreeMap<_, _>>();
    let mut values = axes
        .iter()
        .filter_map(|axis| axis_map.get(*axis).copied())
        .collect::<Vec<_>>();

    while values.len() < axes.len() && !axes.is_empty() {
        values.push(model.overall_score / 100.0);
    }

    let raw = if values.is_empty() {
        model.overall_score
    } else {
        values.iter().sum::<f64>() / values.len() as f64 * 100.0
    };

    apply_variance_penalty(raw, model.standard_error)
}

fn apply_variance_penalty(score: f64, standard_error: f64) -> f64 {
    let standard_error = standard_error.max(0.0);
    if standard_error == 0.0 {
        return score.clamp(0.0, 100.0);
    }

    let mut penalty = standard_error * STANDARD_ERROR_PENALTY_MULTIPLIER;
    if standard_error >= HIGH_VARIANCE_STANDARD_ERROR {
        penalty += HIGH_VARIANCE_EXTRA_PENALTY;
    }

    (score - penalty).clamp(0.0, 100.0)
}

