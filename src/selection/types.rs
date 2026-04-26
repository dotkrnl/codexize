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

#[derive(Debug, Clone, PartialEq)]
pub struct CachedModel {
    pub vendor: VendorKind,
    pub name: String,
    pub overall_score: f64,
    pub current_score: f64,
    pub standard_error: f64,
    pub axes: Vec<(String, f64)>,
    pub quota_percent: Option<u8>,
    pub display_order: usize,
    /// Sibling whose ranking-API score was borrowed because this model
    /// has no entry yet. `None` for normal models.
    pub fallback_from: Option<String>,
}

impl CachedModel {
    pub fn axis(&self, key: &str) -> Option<f64> {
        self.axes
            .iter()
            .find(|(axis_key, _)| axis_key == key)
            .map(|(_, value)| *value)
    }
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

    fn sample_cached_model() -> CachedModel {
        CachedModel {
            vendor: VendorKind::Codex,
            name: "gpt-5.5".to_string(),
            overall_score: 88.4,
            current_score: 86.2,
            standard_error: 2.9,
            axes: vec![
                ("correctness".to_string(), 90.0),
                ("debugging".to_string(), 82.0),
            ],
            quota_percent: Some(73),
            display_order: 2,
            fallback_from: Some("gpt-5".to_string()),
        }
    }

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

    #[test]
    fn cached_model_axis_returns_matching_value() {
        let model = sample_cached_model();

        assert_eq!(model.axis("correctness"), Some(90.0));
    }

    #[test]
    fn cached_model_axis_returns_none_for_missing_key() {
        let model = sample_cached_model();

        assert_eq!(model.axis("safety"), None);
    }

    #[test]
    fn cached_model_clone_and_fields_remain_accessible() {
        let model = sample_cached_model();
        let cloned = model.clone();

        assert_eq!(cloned, model);
        assert_eq!(cloned.vendor, VendorKind::Codex);
        assert_eq!(cloned.name, "gpt-5.5");
        assert_eq!(cloned.overall_score, 88.4);
        assert_eq!(cloned.current_score, 86.2);
        assert_eq!(cloned.standard_error, 2.9);
        assert_eq!(cloned.quota_percent, Some(73));
        assert_eq!(cloned.display_order, 2);
        assert_eq!(cloned.fallback_from.as_deref(), Some("gpt-5"));
    }
}
