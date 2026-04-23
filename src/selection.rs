use crate::{claude, codex, dashboard, gemini, kimi};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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
pub struct ModelStatus {
    pub vendor: VendorKind,
    pub name: String,
    pub stupid_level: Option<u8>,
    pub quota_percent: Option<u8>,
    pub idea_rank: u8,
    pub planning_rank: u8,
    pub build_rank: u8,
    pub review_rank: u8,
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

pub fn load_all_models() -> Vec<ModelStatus> {
    let dashboard_models = match dashboard::load_models() {
        Ok(models) => models,
        Err(_) => return Vec::new(),
    };

    let quotas = load_quota_maps();
    let mut candidates = dashboard_models
        .into_iter()
        .filter_map(|model| build_candidate(model, &quotas))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return Vec::new();
    }

    let retained_names = top_model_union(&candidates);
    candidates.retain(|candidate| retained_names.contains(&candidate.name));

    let idea_ranks = rank_map(&candidates, |candidate| candidate.idea_probability);
    let planning_ranks = rank_map(&candidates, |candidate| candidate.planning_probability);
    let build_ranks = rank_map(&candidates, |candidate| candidate.build_probability);
    let review_ranks = rank_map(&candidates, |candidate| candidate.review_probability);

    candidates.sort_by(|left, right| compare_candidates(left, right));

    candidates
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
        })
        .collect()
}

fn load_quota_maps() -> BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> {
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
        rx.into_iter().collect()
    })
}

fn load_quota_map_for_vendor(vendor: VendorKind) -> BTreeMap<String, Option<u8>> {
    match vendor {
        VendorKind::Codex => codex::load_live_models()
            .map(live_map_codex)
            .unwrap_or_default(),
        VendorKind::Claude => claude::load_live_models()
            .map(live_map_claude)
            .unwrap_or_default(),
        VendorKind::Gemini => gemini::load_live_models()
            .map(live_map_direct)
            .unwrap_or_default(),
        VendorKind::Kimi => kimi::load_live_models()
            .map(live_map_kimi)
            .unwrap_or_default(),
    }
}

fn build_candidate(
    model: dashboard::DashboardModel,
    quotas: &BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>,
) -> Option<Candidate> {
    let vendor = vendor_for_dashboard_model(&model)?;
    let quota_percent = quotas
        .get(&vendor)
        .and_then(|models| models.get(&model.name))
        .copied()
        .flatten();

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

fn vendor_for_dashboard_model(model: &dashboard::DashboardModel) -> Option<VendorKind> {
    let vendor = model.vendor.as_str();
    if model.name.starts_with("claude-") || vendor == "anthropic" {
        return Some(VendorKind::Claude);
    }
    if model.name.starts_with("gpt-") || vendor == "openai" {
        return Some(VendorKind::Codex);
    }
    if model.name.starts_with("gemini-") || vendor == "google" {
        return Some(VendorKind::Gemini);
    }
    if model.name.starts_with("kimi-") || vendor == "kimi" || vendor == "moonshotai" {
        return Some(VendorKind::Kimi);
    }
    None
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
    let shared = raw
        .iter()
        .filter(|(name, _)| !name.contains("spark"))
        .find_map(|(_, quota)| *quota);
    let spark = raw.get("gpt-5.3-codex-spark").copied().flatten();

    let mut mapped = BTreeMap::new();
    for name in raw.keys() {
        let quota = if name.contains("spark") { spark } else { shared };
        mapped.insert(name.clone(), quota);
    }
    mapped
}

fn live_map_claude(models: Vec<claude::LiveModel>) -> BTreeMap<String, Option<u8>> {
    let raw = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect::<BTreeMap<_, _>>();
    let shared = raw
        .iter()
        .find(|(name, _)| name.contains("sonnet"))
        .and_then(|(_, quota)| *quota)
        .or_else(|| raw.get("seven_day").copied().flatten())
        .or_else(|| raw.get("five_hour").copied().flatten());

    BTreeMap::from([
        ("claude-opus-4.1".to_string(), shared),
        ("claude-sonnet-4-5-20250929".to_string(), shared),
        ("claude-haiku-3.5".to_string(), shared),
    ])
}

fn live_map_direct<T: LiveModelLike>(models: Vec<T>) -> BTreeMap<String, Option<u8>> {
    models
        .into_iter()
        .map(|model| (model.name().to_ascii_lowercase(), model.quota_percent()))
        .collect()
}

fn live_map_kimi(models: Vec<kimi::LiveModel>) -> BTreeMap<String, Option<u8>> {
    let mut mapped = live_map_direct(models);
    if let Some(quota) = mapped.get("kimi-latest").copied().flatten().or_else(|| mapped.get("kimi").copied().flatten()) {
        mapped.insert("kimi-latest".to_string(), Some(quota));
        mapped.insert("kimi-code".to_string(), Some(quota));
        mapped.insert("kimi-for-coding".to_string(), Some(quota));
    }
    mapped
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
