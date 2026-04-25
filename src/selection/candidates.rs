use super::config::{SELECTION_CONFIG, SelectionPhase};
use super::quota;
use super::ranking;
use super::types::{Candidate, ModelStatus, QuotaError, TaskKind, VendorKind};
use super::vendor;
use crate::dashboard;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

#[cfg(test)]
static TEST_SAMPLE_SEED: AtomicU64 = AtomicU64::new(0);

pub fn load_all_models() -> (Vec<ModelStatus>, Vec<QuotaError>) {
    let mut dashboard_models = match dashboard::load_models() {
        Ok(models) => models,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let (quotas, errors) = quota::load_quota_maps();

    // Synthesize entries for live-quota models missing from the ranking API
    // (e.g. "gpt-5.5" before aistupidlevel picks it up). Uses same-stem
    // siblings' scores; once the real score lands, the name matches here and
    // this synthesis is skipped.
    let existing: HashSet<String> = dashboard_models.iter().map(|m| m.name.clone()).collect();
    let mut synthesized: HashSet<String> = HashSet::new();
    for (vendor_kind, models) in &quotas {
        let vendor_str = vendor::vendor_kind_to_str(*vendor_kind);
        for name in models.keys() {
            if existing.contains(name) || synthesized.contains(name) {
                continue;
            }
            if let Some(model) = dashboard::synthesize_sibling(name, vendor_str, &dashboard_models) {
                synthesized.insert(name.clone());
                dashboard_models.push(model);
            }
        }
    }

    let mut candidates = dashboard_models
        .into_iter()
        .filter_map(|model| build_candidate(model, &quotas))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return (Vec::new(), errors);
    }

    // Collapse all Kimi candidates into a single "kimi-latest" entry using
    // the best-scoring kimi as the source (the kimi CLI only has one model).
    let best_kimi_idx = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.vendor == VendorKind::Kimi)
        .max_by(|(_, a), (_, b)| {
            a.overall_score
                .partial_cmp(&b.overall_score)
                .unwrap_or(Ordering::Equal)
        })
        .map(|(i, _)| i);
    if let Some(i) = best_kimi_idx {
        let mut canonical = candidates[i].clone();
        canonical.name = "kimi-latest".to_string();
        candidates.retain(|c| c.vendor != VendorKind::Kimi);
        candidates.push(canonical);
    }

    // Order matters:
    //   1. Apply version penalties so synthesized newer-version models
    //      (e.g. gemini-3-flash-preview borrowing 2.5-flash's score) outrank
    //      their sources before later filtering looks at probability.
    //   2. Zero out below-third probabilities so cutoff-losers don't
    //      occupy a top_model_union "top-N by probability" slot.
    //   3. THEN take top_model_union — its "best per vendor by
    //      overall_score" rule still keeps every vendor represented even
    //      when all that vendor's probabilities were cut to zero.
    apply_version_penalties(&mut candidates);
    apply_top_third_cutoff(&mut candidates);

    let retained_names = ranking::top_model_union(&candidates);
    candidates.retain(|candidate| retained_names.contains(&candidate.name));

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
            fallback_from: candidate.fallback_from.clone(),
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
    used: &[crate::state::RunRecord],
) -> Option<&'a ModelStatus> {
    let used_vendors: Vec<VendorKind> = used
        .iter()
        .filter_map(|r| vendor::str_to_vendor(&r.vendor))
        .collect();
    let used_names: Vec<&str> = used.iter().map(|r| r.model.as_str()).collect();

    // 1. Different vendor AND different model
    let fresh_vendor: Vec<&ModelStatus> = models
        .iter()
        .filter(|m| !used_vendors.contains(&m.vendor) && !used_names.contains(&m.name.as_str()))
        .collect();
    if let Some(m) = weighted_sample(&fresh_vendor, TaskKind::Review) {
        return Some(m);
    }

    // 2. Same vendor but different model
    let fresh_model: Vec<&ModelStatus> = models
        .iter()
        .filter(|m| !used_names.contains(&m.name.as_str()))
        .collect();
    weighted_sample(&fresh_model, TaskKind::Review)
}

pub fn select_excluding<'a>(
    models: &'a [ModelStatus],
    task: TaskKind,
    exclude: &HashSet<(VendorKind, String)>,
    last_failed_vendor: Option<VendorKind>,
) -> Option<&'a ModelStatus> {
    let mut candidates: Vec<(&ModelStatus, f64)> = models
        .iter()
        .filter(|model| !exclude.contains(&(model.vendor, model.name.clone())))
        .map(|model| {
            let mut weight = weight_for(model, task);
            if last_failed_vendor.is_some_and(|vendor| vendor != model.vendor) {
                weight *= 1.3;
            }
            (model, weight)
        })
        .collect();

    candidates.sort_by(|(left_model, left_weight), (right_model, right_weight)| {
        right_weight
            .partial_cmp(left_weight)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left_model.rank_for(task).cmp(&right_model.rank_for(task)))
            .then_with(|| left_model.name.cmp(&right_model.name))
    });

    weighted_sample_with_weights(&candidates, task)
}

fn weighted_sample<'a>(candidates: &[&'a ModelStatus], task: TaskKind) -> Option<&'a ModelStatus> {
    let weighted: Vec<(&ModelStatus, f64)> = candidates
        .iter()
        .map(|model| (*model, weight_for(model, task)))
        .collect();
    weighted_sample_with_weights(&weighted, task)
}

fn weighted_sample_with_weights<'a>(
    candidates: &[(&'a ModelStatus, f64)],
    task: TaskKind,
) -> Option<&'a ModelStatus> {
    if candidates.is_empty() {
        return None;
    }
    let total: f64 = candidates.iter().map(|(_, weight)| *weight).sum();
    if total <= 0.0 {
        return candidates
            .iter()
            .min_by_key(|(model, _)| model.rank_for(task))
            .map(|(model, _)| *model);
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

fn weight_for(model: &ModelStatus, task: TaskKind) -> f64 {
    match task {
        TaskKind::Idea => model.idea_weight,
        TaskKind::Planning => model.planning_weight,
        TaskKind::Build => model.build_weight,
        TaskKind::Review => model.review_weight,
    }
}

/// Probabilistically select a model for the given task using weighted probabilities.
pub fn select(models: &[ModelStatus], task: TaskKind) -> Option<&ModelStatus> {
    let all: Vec<&ModelStatus> = models.iter().collect();
    weighted_sample(&all, task)
}

/// Extract the first `xx[.|-yy]` version from a model name where each component
/// is at most 2 digits. Longer digit runs (e.g. dates like 20250514) are skipped.
/// Both `.` (e.g. `gpt-5.5`, `gemini-2.5-flash`) and `-` (e.g. `claude-sonnet-4-6`)
/// count as the major-minor separator.
/// Returns (major, minor) if both components found, or (major, 0) for major-only.
pub(super) fn extract_version(name: &str) -> Option<(u32, u32)> {
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

            // Optionally consume a `[.|-]yy` minor component (1-2 digits)
            if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'.') {
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
/// Versions are bucketed per-vendor so e.g. claude-opus-4-7 isn't ranked
/// "older" than gpt-5.5 just because the integer 4 < 5 across families.
/// Within a vendor, unique versions are ranked newest-first; same version =
/// same penalty.
fn apply_version_penalties(candidates: &mut [Candidate]) {
    let versions: Vec<Option<(u32, u32)>> = candidates
        .iter()
        .map(|c| extract_version(&c.name))
        .collect();

    // Build a per-vendor list of unique versions, newest-first.
    let mut per_vendor: BTreeMap<VendorKind, Vec<(u32, u32)>> = BTreeMap::new();
    for (candidate, version) in candidates.iter().zip(versions.iter()) {
        if let Some(v) = version {
            per_vendor.entry(candidate.vendor).or_default().push(*v);
        }
    }
    for unique in per_vendor.values_mut() {
        unique.sort_unstable_by(|a, b| b.cmp(a));
        unique.dedup();
    }

    let cfg = &SELECTION_CONFIG;
    for (candidate, version) in candidates.iter_mut().zip(versions.iter()) {
        let Some(unique) = per_vendor.get(&candidate.vendor) else {
            continue;
        };
        if unique.len() <= 1 {
            continue;
        }
        let rank = version
            .and_then(|v| unique.iter().position(|u| *u == v))
            .unwrap_or(0);
        let interactive_penalty = cfg.version_penalty_per_step_interactive.powi(rank as i32);
        let headless_penalty = cfg.version_penalty_per_step_headless.powi(rank as i32);
        candidate.idea_probability *= interactive_penalty;
        candidate.planning_probability *= interactive_penalty;
        candidate.build_probability *= headless_penalty;
        candidate.review_probability *= headless_penalty;
    }
}

/// Zero out any probability that's below one third of the top probability
/// in its phase. Keeps the top tier competing on weight and hard-excludes
/// trailing models from both weighted sampling and round-robin fallbacks.
fn apply_top_third_cutoff(candidates: &mut [Candidate]) {
    fn cutoff<F: Fn(&Candidate) -> f64>(candidates: &[Candidate], selector: F) -> f64 {
        candidates.iter().map(selector).fold(0.0_f64, f64::max) / 3.0
    }

    let idea_cut = cutoff(candidates, |c| c.idea_probability);
    let planning_cut = cutoff(candidates, |c| c.planning_probability);
    let build_cut = cutoff(candidates, |c| c.build_probability);
    let review_cut = cutoff(candidates, |c| c.review_probability);

    for c in candidates.iter_mut() {
        if c.idea_probability < idea_cut {
            c.idea_probability = 0.0;
        }
        if c.planning_probability < planning_cut {
            c.planning_probability = 0.0;
        }
        if c.build_probability < build_cut {
            c.build_probability = 0.0;
        }
        if c.review_probability < review_cut {
            c.review_probability = 0.0;
        }
    }
}

fn is_flash_tier(candidate: &Candidate) -> bool {
    candidate.vendor == VendorKind::Gemini
        && (candidate.name.contains("flash") || candidate.name.contains("nano"))
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

    let mut candidate = Candidate {
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
        fallback_from: model.fallback_from,
    };

    if is_flash_tier(&candidate) {
        let penalty = SELECTION_CONFIG.flash_tier_penalty;
        candidate.idea_probability *= penalty;
        candidate.planning_probability *= penalty;
        candidate.build_probability *= penalty;
        candidate.review_probability *= penalty;
    }

    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::collections::HashSet;

    fn sample_model_status() -> ModelStatus {
        ModelStatus {
            vendor: VendorKind::Claude,
            name: "claude-sonnet".to_string(),
            stupid_level: Some(7),
            quota_percent: Some(80),
            idea_rank: 4,
            planning_rank: 3,
            build_rank: 2,
            review_rank: 1,
            idea_weight: 0.6,
            planning_weight: 0.5,
            build_weight: 0.4,
            review_weight: 0.3,
            fallback_from: None,
        }
    }

    #[test]
    fn idea_task_uses_idea_weight() {
        let model = sample_model_status();

        assert_eq!(weight_for(&model, TaskKind::Idea), 0.6);
    }

    #[test]
    fn select_uses_idea_weights_for_idea_task() {
        let mut idea_choice = sample_model_status();
        idea_choice.name = "idea-choice".to_string();
        idea_choice.idea_weight = 1.0;
        idea_choice.build_weight = 0.0;
        idea_choice.planning_weight = 0.0;
        idea_choice.review_weight = 0.0;

        let mut build_choice = sample_model_status();
        build_choice.name = "build-choice".to_string();
        build_choice.idea_weight = 0.0;
        build_choice.build_weight = 1.0;
        build_choice.planning_weight = 1.0;
        build_choice.review_weight = 1.0;

        let models = vec![idea_choice, build_choice];

        let chosen = select(&models, TaskKind::Idea).expect("expected idea task selection");

        assert_eq!(chosen.name, "idea-choice");
    }

    #[test]
    fn select_excluding_returns_none_for_empty_models() {
        let excluded = HashSet::new();

        let chosen = select_excluding(&[], TaskKind::Build, &excluded, None);

        assert!(chosen.is_none());
    }

    #[test]
    fn select_excluding_returns_none_when_everything_excluded() {
        let models = vec![sample_model_status()];
        let excluded = HashSet::from([(VendorKind::Claude, "claude-sonnet".to_string())]);

        let chosen = select_excluding(&models, TaskKind::Build, &excluded, None);

        assert!(chosen.is_none());
    }

    #[test]
    fn select_excluding_applies_diversity_bonus() {
        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let models = vec![
            ModelStatus {
                vendor: VendorKind::Claude,
                name: "same-vendor".to_string(),
                build_weight: 1.0,
                ..sample_model_status()
            },
            ModelStatus {
                vendor: VendorKind::Gemini,
                name: "other-vendor".to_string(),
                build_weight: 0.8,
                ..sample_model_status()
            },
        ];
        let excluded = HashSet::new();

        let chosen = select_excluding(
            &models,
            TaskKind::Build,
            &excluded,
            Some(VendorKind::Claude),
        )
        .expect("expected a choice");

        assert_eq!(chosen.name, "other-vendor");
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn select_excluding_keeps_same_vendor_candidates_when_needed() {
        let models = vec![sample_model_status()];
        let excluded = HashSet::new();

        let chosen = select_excluding(
            &models,
            TaskKind::Build,
            &excluded,
            Some(VendorKind::Claude),
        )
        .expect("expected a same-vendor candidate");

        assert_eq!(chosen.name, "claude-sonnet");
    }

    fn dashboard_model(name: &str) -> dashboard::DashboardModel {
        dashboard::DashboardModel {
            name: name.to_string(),
            vendor: "gemini".to_string(),
            overall_score: 75.0,
            current_score: 75.0,
            standard_error: 0.0,
            axes: Vec::new(),
            display_order: 0,
            fallback_from: None,
        }
    }

    fn quotas_for(
        entries: &[(&str, Option<u8>)],
    ) -> BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> {
        let mut inner = BTreeMap::new();
        for (name, quota) in entries {
            inner.insert((*name).to_string(), *quota);
        }
        BTreeMap::from([(VendorKind::Gemini, inner)])
    }

    fn model_status_from_candidate(candidate: &Candidate) -> ModelStatus {
        ModelStatus {
            vendor: candidate.vendor,
            name: candidate.name.clone(),
            stupid_level: candidate.stupid_level,
            quota_percent: candidate.quota_percent,
            idea_rank: 1,
            planning_rank: 1,
            build_rank: 1,
            review_rank: 1,
            idea_weight: candidate.idea_probability,
            planning_weight: candidate.planning_probability,
            build_weight: candidate.build_probability,
            review_weight: candidate.review_probability,
            fallback_from: candidate.fallback_from.clone(),
        }
    }

    #[test]
    fn flash_model_gets_penalty() {
        let flash_name = "gemini-2.5-flash";
        let pro_name = "gemini-2.5-pro";
        let flash_model = dashboard_model(flash_name);
        let pro_model = dashboard_model(pro_name);
        let quotas = quotas_for(&[(flash_name, Some(80)), (pro_name, Some(80))]);

        let flash = build_candidate(flash_model.clone(), &quotas).expect("flash candidate");
        let pro = build_candidate(pro_model.clone(), &quotas).expect("pro candidate");

        let penalty = SELECTION_CONFIG.flash_tier_penalty;
        let epsilon = 1e-12;

        assert!((flash.idea_probability - pro.idea_probability * penalty).abs() < epsilon);
        assert!((flash.planning_probability - pro.planning_probability * penalty).abs() < epsilon);
        assert!((flash.build_probability - pro.build_probability * penalty).abs() < epsilon);
        assert!((flash.review_probability - pro.review_probability * penalty).abs() < epsilon);
    }

    #[test]
    fn non_flash_gemini_unaffected() {
        let pro_name = "gemini-2.5-pro";
        let pro_model = dashboard_model(pro_name);
        let quotas = quotas_for(&[(pro_name, Some(80))]);

        let pro = build_candidate(pro_model.clone(), &quotas).expect("pro candidate");
        let epsilon = 1e-12;

        let expected_idea = ranking::selection_probability(
            &pro_model,
            Some(80),
            VendorKind::Gemini,
            SelectionPhase::Idea,
        );
        let expected_planning = ranking::selection_probability(
            &pro_model,
            Some(80),
            VendorKind::Gemini,
            SelectionPhase::Planning,
        );
        let expected_build = ranking::selection_probability(
            &pro_model,
            Some(80),
            VendorKind::Gemini,
            SelectionPhase::Build,
        );
        let expected_review = ranking::selection_probability(
            &pro_model,
            Some(80),
            VendorKind::Gemini,
            SelectionPhase::Review,
        );

        assert!((pro.idea_probability - expected_idea).abs() < epsilon);
        assert!((pro.planning_probability - expected_planning).abs() < epsilon);
        assert!((pro.build_probability - expected_build).abs() < epsilon);
        assert!((pro.review_probability - expected_review).abs() < epsilon);
    }

    #[test]
    fn flash_survives_as_last_resort() {
        TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
        let flash_name = "gemini-2.5-flash";
        let pro_name = "gemini-2.5-pro";
        let flash_model = dashboard_model(flash_name);
        let pro_model = dashboard_model(pro_name);

        // Only the flash model has quota; the premium model gets zeroed weights.
        let quotas = quotas_for(&[(flash_name, Some(80)), (pro_name, Some(0))]);

        let mut candidates = vec![
            build_candidate(pro_model, &quotas).expect("pro candidate"),
            build_candidate(flash_model, &quotas).expect("flash candidate"),
        ];
        apply_top_third_cutoff(&mut candidates);

        let models: Vec<ModelStatus> = candidates.iter().map(model_status_from_candidate).collect();

        let chosen = select(&models, TaskKind::Build).expect("expected a choice");
        assert_eq!(chosen.name, flash_name);
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn flash_excluded_when_premium_available() {
        let flash_name = "gemini-2.5-flash";
        let pro_name = "gemini-2.5-pro";
        let flash_model = dashboard_model(flash_name);
        let pro_model = dashboard_model(pro_name);
        let quotas = quotas_for(&[(flash_name, Some(80)), (pro_name, Some(80))]);

        let mut candidates = vec![
            build_candidate(flash_model, &quotas).expect("flash candidate"),
            build_candidate(pro_model, &quotas).expect("pro candidate"),
        ];
        apply_top_third_cutoff(&mut candidates);

        let flash = candidates
            .iter()
            .find(|c| c.name == flash_name)
            .expect("flash still present");
        assert_eq!(flash.build_probability, 0.0);

        let models: Vec<ModelStatus> = candidates.iter().map(model_status_from_candidate).collect();

        for seed in 1..50_u64 {
            TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
            let chosen = select(&models, TaskKind::Build).expect("expected a choice");
            assert_ne!(chosen.name, flash_name);
        }
        TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
    }

    #[test]
    fn extract_version_treats_dot_as_minor_separator() {
        assert_eq!(extract_version("gpt-5.5"), Some((5, 5)));
        assert_eq!(extract_version("gpt-5.4"), Some((5, 4)));
        assert_eq!(extract_version("gpt-5.2"), Some((5, 2)));
        assert_eq!(extract_version("gemini-2.5-flash"), Some((2, 5)));
        assert_eq!(extract_version("gemini-3-pro-preview"), Some((3, 0)));
        assert_eq!(extract_version("gemini-3-flash-preview"), Some((3, 0)));
        // Existing dash-separated minor still works.
        assert_eq!(extract_version("claude-sonnet-4-6"), Some((4, 6)));
        // Date-shaped digit runs are skipped.
        assert_eq!(extract_version("gpt-4-turbo-2024-04-09"), Some((4, 0)));
    }

    fn candidate_with_version(name: &str, score: f64) -> Candidate {
        Candidate {
            vendor: VendorKind::Codex,
            name: name.to_string(),
            stupid_level: Some(50),
            quota_percent: Some(80),
            overall_score: score,
            display_order: 0,
            idea_probability: 1.0,
            planning_probability: 1.0,
            build_probability: 1.0,
            review_probability: 1.0,
            fallback_from: None,
        }
    }

    #[test]
    fn synthesized_gpt_5_5_makes_5_4_carry_version_penalty() {
        // gpt-5.5 is synthesized (borrows gpt-5.2's score) but should still
        // count as the newest version. gpt-5.4 must end up with a smaller
        // weight than gpt-5.5 thanks to apply_version_penalties.
        let mut candidates = vec![
            candidate_with_version("gpt-5.5", 70.0),
            candidate_with_version("gpt-5.4", 70.0),
            candidate_with_version("gpt-5.2", 70.0),
        ];
        apply_version_penalties(&mut candidates);

        let by_name = |n: &str| {
            candidates
                .iter()
                .find(|c| c.name == n)
                .unwrap()
                .build_probability
        };
        assert!(by_name("gpt-5.5") > by_name("gpt-5.4"));
        assert!(by_name("gpt-5.4") > by_name("gpt-5.2"));
    }

    #[test]
    fn version_penalty_does_not_cross_vendors() {
        // claude-opus-4-7 must not be penalised relative to gpt-5.5 just
        // because gpt's integer is bigger — different families.
        let mut candidates = vec![
            candidate_with_version("gpt-5.5", 70.0),
            candidate_with_version("gpt-5.4", 70.0),
            Candidate {
                vendor: VendorKind::Claude,
                ..candidate_with_version("claude-opus-4-7", 70.0)
            },
        ];
        apply_version_penalties(&mut candidates);

        let by_name = |n: &str| {
            candidates
                .iter()
                .find(|c| c.name == n)
                .unwrap()
                .build_probability
        };
        // Only one claude version exists in the pool → no penalty applied.
        assert_eq!(by_name("claude-opus-4-7"), 1.0);
        // gpt-5.5 still wins within Codex.
        assert!(by_name("gpt-5.5") > by_name("gpt-5.4"));
    }

    #[test]
    fn synthesized_gemini_3_flash_preview_makes_2_5_flash_carry_penalty() {
        // gemini-3-flash-preview is synthesized off gemini-2.5-flash via the
        // explicit fallback. Within the gemini bucket it must end up newer
        // (rank 0) and the 2.5 source carries the per-step penalty.
        let mut candidates = vec![
            Candidate {
                vendor: VendorKind::Gemini,
                ..candidate_with_version("gemini-3-flash-preview", 60.0)
            },
            Candidate {
                vendor: VendorKind::Gemini,
                ..candidate_with_version("gemini-2.5-flash", 60.0)
            },
        ];
        apply_version_penalties(&mut candidates);

        let new_flash = candidates
            .iter()
            .find(|c| c.name == "gemini-3-flash-preview")
            .unwrap();
        let old_flash = candidates
            .iter()
            .find(|c| c.name == "gemini-2.5-flash")
            .unwrap();
        assert!(new_flash.idea_probability > old_flash.idea_probability);
        assert!(new_flash.build_probability > old_flash.build_probability);
    }
}
