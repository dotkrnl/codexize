use crate::{claude, codex, gemini, kimi};
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
        "gpt-5.3-codex-spark" => (3, 3, 2, 4),
        _ => (9, 9, 9, 9),
    }
}

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
        Ok(models) if !models.is_empty() => models
            .into_iter()
            .map(|model| {
                let (idea_rank, planning_rank, build_rank, review_rank) =
                    ranks_for_model(&model.name);
                ModelStatus::new(
                    VendorKind::Codex,
                    model.name,
                    None,
                    model.quota_percent,
                    idea_rank,
                    planning_rank,
                    build_rank,
                    review_rank,
                )
            })
            .collect(),
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
        Ok(models) if !models.is_empty() => models
            .into_iter()
            .map(|model| {
                let (idea_rank, planning_rank, build_rank, review_rank) =
                    ranks_for_model(&model.name);
                ModelStatus::new(
                    VendorKind::Claude,
                    model.name,
                    None,
                    model.quota_percent,
                    idea_rank,
                    planning_rank,
                    build_rank,
                    review_rank,
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn load_gemini_models() -> Vec<ModelStatus> {
    match gemini::load_live_models() {
        Ok(models) if !models.is_empty() => models
            .into_iter()
            .map(|model| {
                let (idea_rank, planning_rank, build_rank, review_rank) =
                    ranks_for_model(&model.name);
                ModelStatus::new(
                    VendorKind::Gemini,
                    model.name,
                    None,
                    model.quota_percent,
                    idea_rank,
                    planning_rank,
                    build_rank,
                    review_rank,
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn load_kimi_models() -> Vec<ModelStatus> {
    match kimi::load_live_models() {
        Ok(models) if !models.is_empty() => models
            .into_iter()
            .map(|model| {
                let (idea_rank, planning_rank, build_rank, review_rank) =
                    ranks_for_model(&model.name);
                ModelStatus::new(
                    VendorKind::Kimi,
                    model.name,
                    None,
                    model.quota_percent,
                    idea_rank,
                    planning_rank,
                    build_rank,
                    review_rank,
                )
            })
            .collect(),
        _ => Vec::new(),
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
