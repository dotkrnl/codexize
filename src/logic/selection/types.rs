use serde::{Deserialize, Serialize};

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

/// Origin of the per-phase rank scores carried on a model.
///
/// `Ipbr` is the only value that authorizes automatic phase selection or
/// any selection-affecting ordering. `Aistupidlevel` and `None` mark
/// non-authoritative state: cosmetic display only. Cosmetic
/// `overall_score` / `current_score` and legacy aistupidlevel `axes` MUST
/// NOT be backfilled into ipbr phase scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScoreSource {
    /// No score data is associated with this model. This is the default
    /// for new fields until task 2 lands ipbr ingestion.
    #[default]
    None,
    /// Per-phase ipbr rank scores are authoritative for ranking and
    /// selection.
    Ipbr,
    /// Cosmetic aistupidlevel summary scores only — never an ipbr phase
    /// fallback.
    Aistupidlevel,
}

/// Per-phase ipbr rank scores. Each field corresponds to one ipbr
/// scoreboard column: Idea = `i_adj`, Planning = `p_adj`, Build = `b_adj`,
/// Review = `r`.
///
/// `None` means the matched ipbr row did not provide that phase score, in
/// which case selection MUST exclude the model from auto-selection for
/// that phase rather than backfilling from `overall_score`, the legacy
/// aistupidlevel axes, or any sibling synthesis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct IpbrPhaseScores {
    pub idea: Option<f64>,
    pub planning: Option<f64>,
    pub build: Option<f64>,
    pub review: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CachedModel {
    pub vendor: VendorKind,
    pub name: String,
    /// Cosmetic display-only summary score. MUST NOT drive phase ranking,
    /// auto-selection eligibility, or vendor backfill ordering.
    pub overall_score: f64,
    /// Cosmetic display-only summary score. Same constraint as
    /// `overall_score`.
    pub current_score: f64,
    pub standard_error: f64,
    /// Values are 0.0..=1.0 floats from the aistupidlevel API; keys are
    /// lowercased camelCase. Backfill semantics are owned by the selection layer.
    pub axes: Vec<(String, f64)>,
    pub axis_provenance: BTreeMap<String, String>,
    /// Per-phase ipbr rank scores. Defaults to all-`None` until ipbr
    /// ingestion lands.
    pub ipbr_phase_scores: IpbrPhaseScores,
    /// Where the per-phase rank scores came from. Defaults to
    /// `ScoreSource::None`. Selection MUST treat anything other than
    /// `Ipbr` as unranked.
    pub score_source: ScoreSource,
    /// `true` when this model matched an ipbr row by normalized exact key;
    /// `false` for inventory-/CLI-only visible models. Distinguishes
    /// "matched ipbr row but missing phase score" (still eligible for
    /// other phases) from "no ipbr row at all".
    pub ipbr_row_matched: bool,
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
            ipbr_phase_scores: IpbrPhaseScores::default(),
            score_source: ScoreSource::None,
            ipbr_row_matched: false,
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

    #[test]
    fn new_ipbr_fields_default_to_unscored_and_unmatched() {
        let model = sample_cached_model();

        assert_eq!(model.ipbr_phase_scores, IpbrPhaseScores::default());
        assert_eq!(model.ipbr_phase_scores.idea, None);
        assert_eq!(model.ipbr_phase_scores.planning, None);
        assert_eq!(model.ipbr_phase_scores.build, None);
        assert_eq!(model.ipbr_phase_scores.review, None);
        assert_eq!(model.score_source, ScoreSource::None);
        assert!(!model.ipbr_row_matched);
    }

    #[test]
    fn score_source_default_is_none_not_ipbr() {
        // The default MUST be a non-`Ipbr` value so freshly-constructed
        // entries cannot be mistaken for ipbr-authoritative data.
        let source = ScoreSource::default();
        assert_eq!(source, ScoreSource::None);
        assert_ne!(source, ScoreSource::Ipbr);
    }
}
