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

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct CachedModel {
    pub vendor: VendorKind,
    pub name: String,
    pub overall_score: f64,
    pub current_score: f64,
    pub standard_error: f64,
    /// Values are 0.0..=1.0 floats from the aistupidlevel API; keys are
    /// lowercased camelCase. Backfill semantics are owned by the selection layer.
    pub axes: Vec<(String, f64)>,
    pub axis_provenance: BTreeMap<String, String>,
    pub quota_percent: Option<u8>,
    pub quota_resets_at: Option<chrono::DateTime<chrono::Utc>>,
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
            axis_provenance: BTreeMap::new(),
            quota_percent: Some(73),
            quota_resets_at: None,
            display_order: 2,
            fallback_from: Some("gpt-5".to_string()),
        }
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
