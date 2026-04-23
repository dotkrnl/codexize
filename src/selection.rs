use crate::{claude, codex, gemini, kimi};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

pub fn ranks_for_model(name: &str) -> (u8, u8, u8, u8) {
    match name.to_ascii_lowercase().as_str() {
        "gpt-5.4" => (1, 2, 1, 2),
        "gpt-5.2" => (2, 1, 3, 1),
        "gpt-5.4-mini" => (4, 4, 4, 5),
        "gpt-5.3-codex" => (3, 3, 2, 3),
        "gpt-5.3-codex-spark" => (5, 5, 5, 4),
        "claude-opus-4.1" => (1, 1, 4, 2),
        "claude-sonnet-4-5-20250929" => (2, 2, 2, 1),
        "claude-haiku-3.5" => (6, 6, 6, 6),
        "gemini-3-pro-preview" => (4, 4, 5, 5),
        "kimi-latest" => (5, 5, 3, 4),
        _ => (9, 9, 9, 9),
    }
}

const CODEX_SHARED_MODELS: &[&str] = &["gpt-5.4", "gpt-5.4-mini", "gpt-5.3-codex", "gpt-5.2"];
const CLAUDE_SHARED_MODELS: &[&str] = &[
    "claude-opus-4.1",
    "claude-sonnet-4-5-20250929",
    "claude-haiku-3.5",
];
const GEMINI_CANONICAL_MODELS: &[&str] = &["gemini-3-pro-preview"];
const KIMI_CANONICAL_MODELS: &[&str] = &["kimi-latest"];

impl ModelStatus {
    pub fn new(
        vendor: VendorKind,
        name: impl Into<String>,
        stupid_level: Option<u8>,
        quota_percent: Option<u8>,
        idea_rank: u8,
        planning_rank: u8,
        build_rank: u8,
        review_rank: u8,
    ) -> Self {
        Self {
            vendor,
            name: name.into(),
            stupid_level,
            quota_percent,
            idea_rank,
            planning_rank,
            build_rank,
            review_rank,
        }
    }

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
    let mut all = Vec::new();
    all.extend(load_codex_models());
    all.extend(load_claude_models());
    all.extend(load_gemini_models());
    all.extend(load_kimi_models());
    all.sort_by(|left, right| left.name.cmp(&right.name));
    all
}

pub fn load_codex_models() -> Vec<ModelStatus> {
    match codex::load_live_models() {
        Ok(models) if !models.is_empty() => {
            let live = live_map(models);
            let shared_quota = CODEX_SHARED_MODELS
                .iter()
                .find_map(|name| live.get(*name).copied().flatten());
            let mut rows = CODEX_SHARED_MODELS
                .iter()
                .map(|name| model_status(VendorKind::Codex, name, shared_quota))
                .collect::<Vec<_>>();

            rows.push(model_status(
                VendorKind::Codex,
                "gpt-5.3-codex-spark",
                live.get("gpt-5.3-codex-spark").copied().flatten(),
            ));
            rows
        }
        Ok(_) => vec![ModelStatus::new(
            VendorKind::Codex,
            "codex",
            None,
            None,
            9,
            9,
            9,
            9,
        )],
        Err(error) => vec![ModelStatus::new(
            VendorKind::Codex,
            format!("codex: {}", truncate_error(&error.to_string(), 9)),
            None,
            None,
            9,
            9,
            9,
            9,
        )],
    }
}

pub fn load_claude_models() -> Vec<ModelStatus> {
    match claude::load_live_models() {
        Ok(models) if !models.is_empty() => {
            let live = live_map(models);
            let shared_quota = live
                .iter()
                .find(|(name, _)| name.contains("sonnet"))
                .map(|(_, quota)| *quota)
                .flatten()
                .or_else(|| live.get("seven_day").copied().flatten())
                .or_else(|| live.get("five_hour").copied().flatten());

            CLAUDE_SHARED_MODELS
                .iter()
                .map(|name| model_status(VendorKind::Claude, name, shared_quota))
                .collect()
        }
        _ => Vec::new(),
    }
}

pub fn load_gemini_models() -> Vec<ModelStatus> {
    match gemini::load_live_models() {
        Ok(models) if !models.is_empty() => {
            let live = live_map(models);
            GEMINI_CANONICAL_MODELS
                .iter()
                .map(|name| {
                    let quota = live
                        .get(*name)
                        .copied()
                        .flatten()
                        .or_else(|| find_first_matching_quota(&live, "gemini-3-pro"))
                        .or_else(|| find_first_matching_quota(&live, "gemini-2.5-pro"));
                    model_status(VendorKind::Gemini, name, quota)
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

pub fn load_kimi_models() -> Vec<ModelStatus> {
    match kimi::load_live_models() {
        Ok(models) if !models.is_empty() => {
            let live = live_map(models);
            KIMI_CANONICAL_MODELS
                .iter()
                .map(|name| {
                    let quota = live
                        .get(*name)
                        .copied()
                        .flatten()
                        .or_else(|| find_first_matching_quota(&live, "kimi-latest"))
                        .or_else(|| live.get("kimi").copied().flatten());
                    model_status(VendorKind::Kimi, name, quota)
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn live_map(models: Vec<impl LiveModelLike>) -> BTreeMap<String, Option<u8>> {
    models
        .into_iter()
        .map(|model| (model.name().to_ascii_lowercase(), model.quota_percent()))
        .collect()
}

fn find_first_matching_quota(live: &BTreeMap<String, Option<u8>>, needle: &str) -> Option<u8> {
    live.iter()
        .find(|(name, _)| name.contains(needle))
        .and_then(|(_, quota)| *quota)
}

fn model_status(vendor: VendorKind, name: &str, quota_percent: Option<u8>) -> ModelStatus {
    let (idea_rank, planning_rank, build_rank, review_rank) = ranks_for_model(name);
    ModelStatus::new(
        vendor,
        name,
        None,
        quota_percent,
        idea_rank,
        planning_rank,
        build_rank,
        review_rank,
    )
}

trait LiveModelLike {
    fn name(&self) -> &str;
    fn quota_percent(&self) -> Option<u8>;
}

impl LiveModelLike for codex::LiveModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn quota_percent(&self) -> Option<u8> {
        self.quota_percent
    }
}

impl LiveModelLike for claude::LiveModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn quota_percent(&self) -> Option<u8> {
        self.quota_percent
    }
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

fn truncate_error(text: &str, max_len: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_len).collect::<String>();
    if chars.next().is_some() {
        truncated
    } else {
        text.to_string()
    }
}
