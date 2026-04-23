use crate::{claude, codex, dashboard, gemini, kimi};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MIN_ROLE_SCORE_WEIGHT: f64 = 0.05;
const HIGH_VARIANCE_STANDARD_ERROR: f64 = 5.0;
const HIGH_VARIANCE_EXTRA_PENALTY: f64 = 30.0;
const STANDARD_ERROR_PENALTY_MULTIPLIER: f64 = 2.0;

const IDEA_AXES: &[&str] = &["complexity", "edgecases", "contextawareness", "taskcompletion"];
const PLAN_AXES: &[&str] = &["correctness", "complexity", "edgecases", "stability"];
const BUILD_AXES: &[&str] = &["codequality", "correctness", "debugging", "safety"];
const REVIEW_AXES: &[&str] = &["correctness", "debugging", "edgecases", "safety", "stability"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VendorKind {
    Claude,
    Codex,
    Gemini,
    Kimi,
}

#[derive(Debug, Clone)]
pub struct QuotaError {
    pub vendor: VendorKind,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub vendor: VendorKind,
    pub name: String,
    pub stupid_level: Option<u8>,
    pub quota_percent: Option<u8>,
    pub idea_rank: u8,
    pub planning_rank: u8,
    pub build_rank: u8,
    pub review_rank: u8,
    pub idea_weight: f64,
    pub planning_weight: f64,
    pub build_weight: f64,
    pub review_weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Idea,
    Planning,
    Build,
    Review,
}

#[derive(Debug, Clone)]
struct Candidate {
    vendor: VendorKind,
    name: String,
    stupid_level: Option<u8>,
    quota_percent: Option<u8>,
    overall_score: f64,
    display_order: usize,
    idea_probability: f64,
    planning_probability: f64,
    build_probability: f64,
    review_probability: f64,
}

impl VendorKind {
    pub fn refresh_interval(&self) -> Duration {
        match self {
            Self::Claude => claude::REFRESH_INTERVAL,
            Self::Codex => codex::REFRESH_INTERVAL,
            Self::Gemini => gemini::REFRESH_INTERVAL,
            Self::Kimi => kimi::REFRESH_INTERVAL,
        }
    }
}

impl ModelStatus {
    pub fn rank_for(&self, task: TaskKind) -> u8 {
        match task {
            TaskKind::Idea => self.idea_rank,
            TaskKind::Planning => self.planning_rank,
            TaskKind::Build => self.build_rank,
            TaskKind::Review => self.review_rank,
        }
    }
}

pub fn load_all_models() -> (Vec<ModelStatus>, Vec<QuotaError>) {
    let dashboard_models = match dashboard::load_models() {
        Ok(models) => models,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let (quotas, errors) = load_quota_maps();
    let mut candidates = dashboard_models
        .into_iter()
        .filter_map(|model| build_candidate(model, &quotas))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return (Vec::new(), errors);
    }

    let retained_names = top_model_union(&candidates);
    candidates.retain(|candidate| retained_names.contains(&candidate.name));

    apply_version_penalties(&mut candidates);

    let idea_ranks = rank_map(&candidates, |candidate| candidate.idea_probability);
    let planning_ranks = rank_map(&candidates, |candidate| candidate.planning_probability);
    let build_ranks = rank_map(&candidates, |candidate| candidate.build_probability);
    let review_ranks = rank_map(&candidates, |candidate| candidate.review_probability);

    candidates.sort_by(|left, right| compare_candidates(left, right));

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

/// Probabilistically select a model for the given task from the live candidate pool.
/// Selection weight = the task's selection_probability (quota × role score²).
/// Falls back to the top-ranked model if all weights are zero.
pub fn select(models: &[ModelStatus], task: TaskKind) -> Option<&ModelStatus> {
    if models.is_empty() {
        return None;
    }

    let weights: Vec<f64> = models
        .iter()
        .map(|m| match task {
            TaskKind::Idea => m.idea_weight,
            TaskKind::Planning => m.planning_weight,
            TaskKind::Build => m.build_weight,
            TaskKind::Review => m.review_weight,
        })
        .collect();

    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        // No model has quota — fall back to rank 1
        return models.iter().min_by_key(|m| m.rank_for(task));
    }

    // Simple LCG random from system time (no external dep needed)
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64;
    let r = (seed % 1_000_000.0) / 1_000_000.0 * total;

    let mut cumulative = 0.0;
    for (model, &w) in models.iter().zip(weights.iter()) {
        cumulative += w;
        if r < cumulative {
            return Some(model);
        }
    }

    models.last()
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

/// Apply a 0.7-per-version-step penalty to all probability weights.
/// Unique versions are ranked newest-first; same version = same penalty.
fn apply_version_penalties(candidates: &mut Vec<Candidate>) {
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

    for (candidate, version) in candidates.iter_mut().zip(versions.iter()) {
        let rank = version
            .and_then(|v| unique.iter().position(|u| *u == v))
            .unwrap_or(0);
        let penalty = 0.7f64.powi(rank as i32);
        candidate.idea_probability *= penalty;
        candidate.planning_probability *= penalty;
        candidate.build_probability *= penalty;
        candidate.review_probability *= penalty;
    }
}

fn load_quota_maps() -> (BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>, Vec<QuotaError>) {
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for vendor in [
            VendorKind::Codex,
            VendorKind::Claude,
            VendorKind::Gemini,
            VendorKind::Kimi,
        ] {
            let tx = tx.clone();
            scope.spawn(move || {
                let _ = tx.send((vendor, load_quota_map_for_vendor(vendor)));
            });
        }
        drop(tx);
        let mut maps = BTreeMap::new();
        let mut errors = Vec::new();
        for (vendor, result) in rx {
            match result {
                Ok(map) => { maps.insert(vendor, map); }
                Err(e) => errors.push(QuotaError { vendor, message: e }),
            }
        }
        (maps, errors)
    })
}

fn load_quota_map_for_vendor(vendor: VendorKind) -> Result<BTreeMap<String, Option<u8>>, String> {
    match vendor {
        VendorKind::Codex => codex::load_live_models()
            .map(live_map_codex)
            .map_err(|e| e.to_string()),
        VendorKind::Claude => claude::load_live_models()
            .map(live_map_claude)
            .map_err(|e| e.to_string()),
        VendorKind::Gemini => gemini::load_live_models()
            .map(live_map_direct)
            .map_err(|e| e.to_string()),
        VendorKind::Kimi => kimi::load_live_models()
            .map(live_map_kimi)
            .map_err(|e| e.to_string()),
    }
}

fn build_candidate(
    model: dashboard::DashboardModel,
    quotas: &BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>,
) -> Option<Candidate> {
    let vendor = vendor_for_dashboard_model(&model)?;

    // Try exact match first
    let quota_percent = quotas
        .get(&vendor)
        .and_then(|models| models.get(&model.name))
        .copied()
        .flatten()
        // If no exact match, use heuristics to find appropriate quota
        .or_else(|| find_quota_by_heuristic(&model.name, vendor, quotas));

    let idea_probability = selection_probability(&model, quota_percent, IDEA_AXES);
    let planning_probability = selection_probability(&model, quota_percent, PLAN_AXES);
    let build_probability = selection_probability(&model, quota_percent, BUILD_AXES);
    let review_probability = selection_probability(&model, quota_percent, REVIEW_AXES);

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

fn find_quota_by_heuristic(
    model_name: &str,
    vendor: VendorKind,
    quotas: &BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>,
) -> Option<u8> {
    let vendor_quotas = quotas.get(&vendor)?;

    // For unknown models, try to find a similar model's quota
    match vendor {
        VendorKind::Codex => {
            // Check if it's a spark variant
            if model_name.contains("spark") || model_name.contains("mini") {
                vendor_quotas.iter()
                    .find(|(name, _)| name.contains("spark"))
                    .and_then(|(_, quota)| *quota)
            } else {
                // Use any non-spark model's quota as shared quota
                vendor_quotas.iter()
                    .find(|(name, _)| !name.contains("spark"))
                    .and_then(|(_, quota)| *quota)
            }
        }
        VendorKind::Claude => {
            // All Claude models typically share quota
            vendor_quotas.values().find_map(|q| *q)
        }
        VendorKind::Gemini => {
            // Check for pro vs flash variants
            if model_name.contains("flash") || model_name.contains("nano") {
                vendor_quotas.iter()
                    .find(|(name, _)| name.contains("flash") || name.contains("nano"))
                    .and_then(|(_, quota)| *quota)
            } else {
                vendor_quotas.iter()
                    .find(|(name, _)| name.contains("pro") || name.contains("ultra"))
                    .and_then(|(_, quota)| *quota)
                    .or_else(|| vendor_quotas.values().find_map(|q| *q))
            }
        }
        VendorKind::Kimi => {
            // All Kimi models typically share quota
            vendor_quotas.values().find_map(|q| *q)
        }
    }
}

fn vendor_for_dashboard_model(model: &dashboard::DashboardModel) -> Option<VendorKind> {
    let name = model.name.as_str();
    let vendor = model.vendor.as_str();

    // Check by model name patterns first
    if name.starts_with("claude-") || name.contains("claude") {
        return Some(VendorKind::Claude);
    }
    if name.starts_with("gpt-") || name.starts_with("o1-") || name.contains("gpt") || name.contains("codex") {
        return Some(VendorKind::Codex);
    }
    if name.starts_with("gemini-") || name.contains("gemini") || name.contains("bison") || name.contains("gecko") {
        return Some(VendorKind::Gemini);
    }
    if name.starts_with("kimi-") || name.contains("kimi") || name.contains("moonshot") {
        return Some(VendorKind::Kimi);
    }

    // Check by vendor name
    match vendor {
        "anthropic" | "claude" => Some(VendorKind::Claude),
        "openai" | "microsoft" | "azure" => Some(VendorKind::Codex),
        "google" | "deepmind" => Some(VendorKind::Gemini),
        "kimi" | "moonshotai" | "moonshot" => Some(VendorKind::Kimi),
        _ => {
            // Additional heuristics for unknown models
            if name.contains("opus") || name.contains("sonnet") || name.contains("haiku") {
                Some(VendorKind::Claude)
            } else if name.contains("turbo") || name.contains("davinci") || name.contains("curie") {
                Some(VendorKind::Codex)
            } else if name.contains("palm") || name.contains("lamda") || name.contains("bison") {
                Some(VendorKind::Gemini)
            } else {
                // Unknown vendor/model — skip rather than misassign quota
                None
            }
        }
    }
}

fn selection_probability(
    model: &dashboard::DashboardModel,
    quota_percent: Option<u8>,
    axes: &[&str],
) -> f64 {
    let quota_weight = quota_percent.unwrap_or(0) as f64 / 100.0;
    if quota_weight <= 0.0 {
        return 0.0;
    }
    let role_score = role_score(model, axes) / 100.0;
    quota_weight * role_score.max(MIN_ROLE_SCORE_WEIGHT).powi(2)
}

fn role_score(model: &dashboard::DashboardModel, axes: &[&str]) -> f64 {
    let axis_map = model.axes.iter().cloned().collect::<BTreeMap<_, _>>();
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

fn top_model_union(candidates: &[Candidate]) -> BTreeSet<String> {
    let mut retained = BTreeSet::new();

    // First, ensure at least one model per vendor
    for vendor in [
        VendorKind::Claude,
        VendorKind::Codex,
        VendorKind::Gemini,
        VendorKind::Kimi,
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

fn rank_map(
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

fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .overall_score
        .partial_cmp(&left.overall_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.display_order.cmp(&right.display_order))
        .then_with(|| left.name.cmp(&right.name))
}

fn live_map_codex(models: Vec<codex::LiveModel>) -> BTreeMap<String, Option<u8>> {
    let raw = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect::<BTreeMap<_, _>>();

    // Find shared quota from any non-spark model that has quota
    let shared = raw
        .iter()
        .filter(|(name, _)| !name.contains("spark"))
        .find_map(|(_, quota)| *quota);

    // Find spark quota
    let spark = raw.get("gpt-5.3-codex-spark").copied().flatten()
        .or_else(|| raw.iter().find(|(name, _)| name.contains("spark")).and_then(|(_, quota)| *quota));

    // Map all known Codex models to appropriate quota
    let mut mapped = BTreeMap::new();

    // Add models we found in live probe
    for name in raw.keys() {
        let quota = if name.contains("spark") { spark } else { shared };
        mapped.insert(name.clone(), quota);
    }

    // Add additional known Codex models that might appear from dashboard
    for known_model in &[
        "gpt-5.3-codex",
        "gpt-5.3-codex-nova",
        "gpt-5.3-codex-terra",
        "gpt-5.3-codex-spark",
        "gpt-5.2-codex",
        "gpt-5-64k",
        "gpt-5",
        "gpt-4o-2025-01-20",
        "gpt-4o-latest",
    ] {
        let model_name = known_model.to_string();
        if !mapped.contains_key(&model_name) {
            let quota = if model_name.contains("spark") { spark } else { shared };
            mapped.insert(model_name, quota);
        }
    }

    mapped
}

fn live_map_claude(models: Vec<claude::LiveModel>) -> BTreeMap<String, Option<u8>> {
    let raw = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect::<BTreeMap<_, _>>();

    // Find shared quota from any Claude model or fallback keys
    let shared = raw
        .iter()
        .find(|(name, _)| name.contains("sonnet") || name.contains("opus") || name.contains("haiku"))
        .and_then(|(_, quota)| *quota)
        .or_else(|| raw.get("seven_day").copied().flatten())
        .or_else(|| raw.get("five_hour").copied().flatten())
        .or_else(|| raw.values().find_map(|q| *q));

    // Map all known Claude models to shared quota
    let mut mapped = BTreeMap::new();

    // Add models we found in live probe
    for name in raw.keys() {
        if name.starts_with("claude-") {
            mapped.insert(name.clone(), shared);
        }
    }

    // Add all known Claude models that might appear from dashboard
    for known_model in &[
        "claude-opus-4.7",
        "claude-opus-4.1",
        "claude-sonnet-4.6",
        "claude-sonnet-4-5-20250929",
        "claude-sonnet-3.5",
        "claude-haiku-4.5",
        "claude-haiku-3.5",
        "claude-3-opus",
        "claude-3-sonnet",
        "claude-3-haiku",
    ] {
        let model_name = known_model.to_string();
        if !mapped.contains_key(&model_name) {
            mapped.insert(model_name, shared);
        }
    }

    mapped
}

fn live_map_direct<T: LiveModelLike>(models: Vec<T>) -> BTreeMap<String, Option<u8>> {
    models
        .into_iter()
        .map(|model| (model.name().to_ascii_lowercase(), model.quota_percent()))
        .collect()
}

fn live_map_kimi(models: Vec<kimi::LiveModel>) -> BTreeMap<String, Option<u8>> {
    // Kimi only has one effective model (kimi-latest); expose it under that
    // canonical name regardless of what the API returns.
    let quota = models
        .into_iter()
        .find_map(|m| m.quota_percent);
    BTreeMap::from([("kimi-latest".to_string(), quota)])
}

trait LiveModelLike {
    fn name(&self) -> &str;
    fn quota_percent(&self) -> Option<u8>;
}

impl LiveModelLike for gemini::LiveModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn quota_percent(&self) -> Option<u8> {
        self.quota_percent
    }
}

impl LiveModelLike for kimi::LiveModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn quota_percent(&self) -> Option<u8> {
        self.quota_percent
    }
}
