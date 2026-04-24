use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::dashboard;
use super::config::{SelectionPhase, SELECTION_CONFIG};
use super::types::{Candidate, ModelStatus, QuotaError, TaskKind, VendorKind};
use super::quota;
use super::ranking;
use super::vendor;

pub fn load_all_models() -> (Vec<ModelStatus>, Vec<QuotaError>) {
    let dashboard_models = match dashboard::load_models() {
        Ok(models) => models,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let (quotas, errors) = quota::load_quota_maps();
    let mut candidates = dashboard_models
        .into_iter()
        .filter_map(|model| build_candidate(model, &quotas))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return (Vec::new(), errors);
    }

    // Collapse all Kimi candidates into a single "kimi-latest" entry using
    // the best-scoring kimi as the source (the kimi CLI only has one model).
    let best_kimi_idx = candidates.iter()
        .enumerate()
        .filter(|(_, c)| c.vendor == VendorKind::Kimi)
        .max_by(|(_, a), (_, b)| {
            a.overall_score.partial_cmp(&b.overall_score).unwrap_or(Ordering::Equal)
        })
        .map(|(i, _)| i);
    if let Some(i) = best_kimi_idx {
        let mut canonical = candidates[i].clone();
        canonical.name = "kimi-latest".to_string();
        candidates.retain(|c| c.vendor != VendorKind::Kimi);
        candidates.push(canonical);
    }

    let retained_names = ranking::top_model_union(&candidates);
    candidates.retain(|candidate| retained_names.contains(&candidate.name));

    apply_version_penalties(&mut candidates);

    let idea_ranks = ranking::rank_map(&candidates, |candidate| candidate.idea_probability);
    let planning_ranks = ranking::rank_map(&candidates, |candidate| candidate.planning_probability);
    let build_ranks = ranking::rank_map(&candidates, |candidate| candidate.build_probability);
    let review_ranks = ranking::rank_map(&candidates, |candidate| candidate.review_probability);

    candidates.sort_by(ranking::compare_candidates);

    let mut statuses: Vec<ModelStatus> = candidates
        .into_iter()
        .map(|candidate| ModelStatus {
            vendor: candidate.vendor,
            name: candidate.name.clone(),
            stupid_level: candidate.stupid_level,
            quota_percent: candidate.quota_percent,
            idea_rank: *idea_ranks.get(&candidate.name).unwrap_or(&99),
            planning_rank: *planning_ranks.get(&candidate.name).unwrap_or(&99),
            build_rank: *build_ranks.get(&candidate.name).unwrap_or(&99),
            review_rank: *review_ranks.get(&candidate.name).unwrap_or(&99),
            idea_weight: candidate.idea_probability,
            planning_weight: candidate.planning_probability,
            build_weight: candidate.build_probability,
            review_weight: candidate.review_probability,
        })
        .collect();

    statuses.sort_by_key(|m| m.build_rank);
    (statuses, errors)
}

/// Select a reviewer excluding all previously used models.
/// Prefers a vendor not yet used; falls back to any unused model.
/// Returns None only if every available model has already reviewed.
pub fn select_for_review<'a>(
    models: &'a [ModelStatus],
    used: &[crate::state::PhaseModel],
) -> Option<&'a ModelStatus> {
    let used_vendors: Vec<VendorKind> = used.iter()
        .filter_map(|r| vendor::str_to_vendor(&r.vendor))
        .collect();
    let used_names: Vec<&str> = used.iter().map(|r| r.model.as_str()).collect();

    // 1. Different vendor AND different model
    let fresh_vendor: Vec<&ModelStatus> = models.iter()
        .filter(|m| !used_vendors.contains(&m.vendor) && !used_names.contains(&m.name.as_str()))
        .collect();
    if let Some(m) = weighted_sample(&fresh_vendor, TaskKind::Review) {
        return Some(m);
    }

    // 2. Same vendor but different model
    let fresh_model: Vec<&ModelStatus> = models.iter()
        .filter(|m| !used_names.contains(&m.name.as_str()))
        .collect();
    weighted_sample(&fresh_model, TaskKind::Review)
}

fn weighted_sample<'a>(candidates: &[&'a ModelStatus], task: TaskKind) -> Option<&'a ModelStatus> {
    if candidates.is_empty() {
        return None;
    }
    let weights: Vec<f64> = candidates
        .iter()
        .map(|m| match task {
            TaskKind::Planning => m.planning_weight,
            TaskKind::Build => m.build_weight,
            TaskKind::Review => m.review_weight,
        })
        .collect();
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return candidates.iter().min_by_key(|m| m.rank_for(task)).copied();
    }
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64;
    let r = (seed % 1_000_000.0) / 1_000_000.0 * total;
    let mut cumulative = 0.0;
    for (model, &w) in candidates.iter().zip(weights.iter()) {
        cumulative += w;
        if r < cumulative {
            return Some(model);
        }
    }
    candidates.last().copied()
}

/// Probabilistically select a model for the given task using weighted probabilities.
pub fn select(models: &[ModelStatus], task: TaskKind) -> Option<&ModelStatus> {
    let all: Vec<&ModelStatus> = models.iter().collect();
    weighted_sample(&all, task)
}

/// Extract the first `xx[-yy]` version from a model name where each component
/// is at most 2 digits. Longer digit runs (e.g. dates like 20250514) are skipped.
/// Returns (major, minor) if both components found, or (major, 0) for major-only.
fn extract_version(name: &str) -> Option<(u32, u32)> {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            // Measure the digit run length
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = i - start;
            if run > 2 {
                // Too many digits (e.g. date) — skip and continue
                continue;
            }
            let major: u32 = name[start..i].parse().ok()?;

            // Optionally consume a `-yy` minor component (1-2 digits)
            if i < bytes.len() && bytes[i] == b'-' {
                let j = i + 1;
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    let mut k = j;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k - j <= 2 {
                        let minor: u32 = name[j..k].parse().ok()?;
                        return Some((major, minor));
                    }
                    // Minor has too many digits — return major-only
                }
            }
            return Some((major, 0));
        } else {
            i += 1;
        }
    }
    None
}

/// Apply a configurable per-version-step penalty to all probability weights.
/// Unique versions are ranked newest-first; same version = same penalty.
fn apply_version_penalties(candidates: &mut [Candidate]) {
    let versions: Vec<Option<(u32, u32)>> = candidates
        .iter()
        .map(|c| extract_version(&c.name))
        .collect();

    // Collect unique versions, sort descending (newest first)
    let mut unique: Vec<(u32, u32)> = versions.iter().filter_map(|v| *v).collect();
    unique.sort_unstable_by(|a, b| b.cmp(a));
    unique.dedup();

    if unique.len() <= 1 {
        return; // nothing to penalise
    }

    let cfg = &SELECTION_CONFIG;
    for (candidate, version) in candidates.iter_mut().zip(versions.iter()) {
        let rank = version
            .and_then(|v| unique.iter().position(|u| *u == v))
            .unwrap_or(0);
        let interactive_penalty = cfg
            .version_penalty_per_step_interactive
            .powi(rank as i32);
        let headless_penalty = cfg.version_penalty_per_step_headless.powi(rank as i32);
        candidate.idea_probability *= interactive_penalty;
        candidate.planning_probability *= interactive_penalty;
        candidate.build_probability *= headless_penalty;
        candidate.review_probability *= headless_penalty;
    }
}

fn build_candidate(
    model: dashboard::DashboardModel,
    quotas: &BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>,
) -> Option<Candidate> {
    let vendor = vendor::vendor_for_dashboard_model(&model)?;

    // Try exact match first
    let quota_percent = quotas
        .get(&vendor)
        .and_then(|models| models.get(&model.name))
        .copied()
        .flatten()
        // If no exact match, use heuristics to find appropriate quota
        .or_else(|| quota::find_quota_by_heuristic(&model.name, vendor, quotas));

    let idea_probability =
        ranking::selection_probability(&model, quota_percent, vendor, SelectionPhase::Idea);
    let planning_probability =
        ranking::selection_probability(&model, quota_percent, vendor, SelectionPhase::Planning);
    let build_probability =
        ranking::selection_probability(&model, quota_percent, vendor, SelectionPhase::Build);
    let review_probability =
        ranking::selection_probability(&model, quota_percent, vendor, SelectionPhase::Review);

    Some(Candidate {
        vendor,
        name: model.name,
        stupid_level: Some(model.current_score.round().clamp(0.0, 99.0) as u8),
        quota_percent,
        overall_score: model.overall_score,
        display_order: model.display_order,
        idea_probability,
        planning_probability,
        build_probability,
        review_probability,
    })
}
