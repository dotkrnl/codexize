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
    Direct,
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

impl CliKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CliKind::Claude => "claude",
            CliKind::Codex => "codex",
            CliKind::Gemini => "gemini",
            CliKind::Kimi => "kimi",
            CliKind::Opencode => "opencode",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(CliKind::Claude),
            "codex" => Some(CliKind::Codex),
            "gemini" => Some(CliKind::Gemini),
            "kimi" => Some(CliKind::Kimi),
            "opencode" => Some(CliKind::Opencode),
            _ => None,
        }
    }

    pub const fn variants() -> &'static [&'static str] {
        &["claude", "codex", "gemini", "kimi", "opencode"]
    }
}
#[derive(Debug, Clone)]
pub struct QuotaError {
    pub subscription: SubscriptionKind,
    pub message: String,
}
/// Origin of the per-stage rank scores carried on a model.
///
/// `Ipbr` is the only value that authorizes automatic stage selection or
/// any selection-affecting ordering. `None` marks the unranked state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScoreSource {
    /// No score data is associated with this model.
    #[default]
    None,
    /// Per-stage ipbr rank scores are authoritative for ranking and
    /// selection.
    Ipbr,
}
/// Per-stage ipbr rank scores. Each field corresponds to one ipbr
/// scoreboard column: Idea = `i_adj`, Planning = `p_adj`, Build = `b_adj`,
/// Review = `r`.
///
/// `None` means the matched ipbr row did not provide that stage score, in
/// which case selection MUST exclude the model from auto-selection for
/// that stage rather than backfilling from any other source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct IpbrStageScores {
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
    /// Per-tuple provider properties resolved from the baked defaults
    /// table (or user `[[providers]]` overrides). Selection consumes
    /// these instead of inferring eligibility from the model name.
    pub enabled: bool,
    pub free: bool,
    pub official: bool,
    pub quota_disabled: bool,
    pub cheap_eligible: bool,
    pub tough_eligible: bool,
    pub effort_eligible: bool,
    pub effort_mapping: crate::data::config::schema::EffortMapping,
    /// `true` when the most recent quota fetch for this candidate's
    /// subscription failed. Selection uses this to apply the spec's
    /// 50% capacity assumption.
    pub quota_failed: bool,
}

impl Candidate {
    /// Per-spec effective quota:
    /// - `quota_disabled` ⇒ Some(100) (forced 100%)
    /// - `free` ⇒ Some(100)
    /// - quota fetched ⇒ Some(value)
    /// - subscription's quota fetch failed ⇒ Some(50)
    /// - none of the above ⇒ None (unknown)
    pub fn effective_quota(&self) -> Option<u8> {
        if self.quota_disabled || self.free {
            return Some(100);
        }
        if let Some(value) = self.quota_percent {
            return Some(value);
        }
        if self.quota_failed {
            return Some(50);
        }
        None
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    /// Display subscription for the selected candidate. Candidate data is
    /// authoritative for launch-time decisions.
    pub subscription: SubscriptionKind,
    pub name: String,
    /// Per-stage ipbr rank scores. `None` per stage means the matched
    /// ipbr row did not provide that stage score.
    pub ipbr_stage_scores: IpbrStageScores,
    /// Where the per-stage rank scores came from. Defaults to
    /// `ScoreSource::None`. Selection MUST treat anything other than
    /// `Ipbr` as unranked.
    pub score_source: ScoreSource,
    pub candidates: Vec<Candidate>,
    pub selected_candidate: Option<usize>,
    pub quota_percent: Option<u8>,
    pub quota_resets_at: Option<chrono::DateTime<chrono::Utc>>,
    pub display_order: usize,
}
pub type CachedModel = ModelRow;
impl ModelRow {
    pub fn selected_candidate(&self) -> Option<&Candidate> {
        self.selected_candidate
            .and_then(|index| self.candidates.get(index))
    }
}
impl SubscriptionKind {
    pub fn direct_cli(self) -> Option<CliKind> {
        match self {
            SubscriptionKind::Claude => Some(CliKind::Claude),
            SubscriptionKind::Codex => Some(CliKind::Codex),
            SubscriptionKind::Gemini => Some(CliKind::Gemini),
            SubscriptionKind::Kimi => Some(CliKind::Kimi),
            SubscriptionKind::OpencodeGo | SubscriptionKind::Direct => None,
        }
    }
}
#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
