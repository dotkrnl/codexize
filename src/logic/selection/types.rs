use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionKind {
    Claude,
    Codex,
    Gemini,
    Kimi,
    #[serde(rename = "opencode-go")]
    OpencodeGo,
    Free,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliKind {
    Claude,
    Codex,
    Gemini,
    Kimi,
    Opencode,
}
#[derive(Debug, Clone)]
pub struct QuotaError {
    pub vendor: SubscriptionKind,
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
pub struct Candidate {
    pub subscription: SubscriptionKind,
    pub cli: CliKind,
    pub launch_name: String,
    pub quota_percent: Option<u8>,
    pub quota_resets_at: Option<chrono::DateTime<chrono::Utc>>,
    pub display_order: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeModelEntry {
    pub mapped_into: String,
    pub cli: CliKind,
    pub model_name: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    /// Compatibility mirror of the selected candidate's subscription for
    /// legacy picker and stage code. Candidate data is authoritative.
    pub vendor: SubscriptionKind,
    pub name: String,
    /// Cosmetic display-only summary score. MUST NOT drive phase ranking,
    /// auto-selection eligibility, or subscription backfill ordering.
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
    /// Stored normalized/canonical ipbr match key. Route subscriptions can expose
    /// different model labels for the same ipbr row, so later dedup must use
    /// this stable key instead of recomputing from display names.
    pub ipbr_match_key: Option<String>,
    pub route_underlying_vendor: Option<SubscriptionKind>,
    pub route_provider: Option<String>,
    pub candidates: Vec<Candidate>,
    pub selected_candidate: Option<usize>,
    pub quota_percent: Option<u8>,
    pub quota_resets_at: Option<chrono::DateTime<chrono::Utc>>,
    pub display_order: usize,
    /// Sibling whose ranking-API score was borrowed because this model
    /// has no entry yet. `None` for normal models.
    pub fallback_from: Option<String>,
}
pub type CachedModel = ModelRow;
impl ModelRow {
    pub fn axis(&self, key: &str) -> Option<f64> {
        self.axes
            .iter()
            .find(|(axis_key, _)| axis_key == key)
            .map(|(_, value)| *value)
    }
    pub fn selected_candidate(&self) -> Option<&Candidate> {
        self.selected_candidate
            .and_then(|index| self.candidates.get(index))
    }
    pub fn selected_cli(&self) -> Option<CliKind> {
        self.selected_candidate().map(|candidate| candidate.cli)
    }
    pub fn selected_launch_name(&self) -> &str {
        self.selected_candidate()
            .map(|candidate| candidate.launch_name.as_str())
            .unwrap_or(&self.name)
    }
}
impl SubscriptionKind {
    pub fn direct_cli(self) -> Option<CliKind> {
        match self {
            SubscriptionKind::Claude => Some(CliKind::Claude),
            SubscriptionKind::Codex => Some(CliKind::Codex),
            SubscriptionKind::Gemini => Some(CliKind::Gemini),
            SubscriptionKind::Kimi => Some(CliKind::Kimi),
            SubscriptionKind::OpencodeGo | SubscriptionKind::Free => None,
        }
    }
}
#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
