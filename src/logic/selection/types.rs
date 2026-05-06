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
#[path = "types_tests.rs"]
mod tests;
