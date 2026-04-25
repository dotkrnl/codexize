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
    /// Sibling whose ranking-API score was borrowed because this model
    /// has no entry yet. `None` for normal models.
    pub fallback_from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Idea,
    Planning,
    Build,
    Review,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub vendor: VendorKind,
    pub name: String,
    pub stupid_level: Option<u8>,
    pub quota_percent: Option<u8>,
    pub overall_score: f64,
    pub display_order: usize,
    pub idea_probability: f64,
    pub planning_probability: f64,
    pub build_probability: f64,
    pub review_probability: f64,
    pub fallback_from: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_model_status() -> ModelStatus {
        ModelStatus {
            vendor: VendorKind::Claude,
            name: "claude-sonnet".to_string(),
            stupid_level: Some(7),
            quota_percent: Some(80),
            idea_rank: 1,
            planning_rank: 2,
            build_rank: 3,
            review_rank: 4,
            idea_weight: 0.4,
            planning_weight: 0.3,
            build_weight: 0.2,
            review_weight: 0.1,
            fallback_from: None,
        }
    }

    #[test]
    fn rank_for_idea_returns_idea_rank() {
        let model = sample_model_status();

        assert_eq!(model.rank_for(TaskKind::Idea), 1);
    }
}
