pub use crate::data::dashboard_model::synthesize_sibling;
use crate::data::dashboard_model::{
    InventoryEntry, ScoreEntry, merge_with_warnings, normalize_ipbr_key,
};
use crate::data::providers::opencode;
use crate::selection::{IpbrPhaseScores, ScoreSource};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
#[cfg(test)]
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
/// Counter events emitted by legacy aistupidlevel axis ingestion. Kept
/// `#[cfg(test)]` because the production score path is now ipbr; only
/// fixture-driven tests still drive these.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestEvent {
    AxisDropped { reason: String },
    AxisParseFail { suite: String, axis: String },
}
#[cfg(test)]
fn ingest_events() -> &'static std::sync::Mutex<Vec<IngestEvent>> {
    use std::sync::{Mutex, OnceLock};
    static EVENTS: OnceLock<Mutex<Vec<IngestEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}
/// Snapshot of every ingest event recorded since the last
/// `clear_ingest_events`. Test-only.
#[cfg(test)]
pub fn ingest_events_snapshot() -> Vec<IngestEvent> {
    // SAFETY: the guarded `Vec` has no panicking mutators, so the mutex
    // cannot be poisoned. The fallback branch is purely defensive.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}
#[cfg(test)]
fn clear_ingest_events() {
    // SAFETY: see `ingest_events_snapshot`.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
}
#[cfg(test)]
fn record_axis_dropped(reason: &str) {
    // SAFETY: see `ingest_events_snapshot`.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(IngestEvent::AxisDropped {
            reason: reason.to_string(),
        });
}
#[cfg(test)]
fn record_axis_parse_fail(suite: &str, axis: &str) {
    // SAFETY: see `ingest_events_snapshot`.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(IngestEvent::AxisParseFail {
            suite: suite.to_string(),
            axis: axis.to_string(),
        });
}
/// Source of truth for the model universe — both the inventory of models
/// and their per-phase rank scores. ipbr is the only upstream feed; opencode
/// inventory is layered on top via the local `opencode` CLI.
pub const IPBR_SCOREBOARD_URL: &str = "https://ipbr.dev/scoreboard.toml";
#[derive(Debug, Clone)]
pub struct DashboardModel {
    pub name: String,
    pub vendor: String,
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
    /// Per-phase ipbr rank scores. Defaults to all-`None` until task 2
    /// lands ipbr ingestion.
    pub ipbr_phase_scores: crate::selection::IpbrPhaseScores,
    /// Where the per-phase rank scores came from. Defaults to
    /// `ScoreSource::None`; only `Ipbr` may drive automatic selection.
    pub score_source: crate::selection::ScoreSource,
    /// `true` when this model matched an ipbr row by normalized exact
    /// key. Inventory-/CLI-only visible models keep this `false`.
    pub ipbr_row_matched: bool,
    pub ipbr_match_key: Option<String>,
    pub route_underlying_vendor: Option<crate::selection::SubscriptionKind>,
    /// Opencode sub-provider this row was advertised under (`opencode` or
    /// `opencode-go`). Carried so the ACP launch boundary can qualify the
    /// bare `name` with the right tier prefix. `None` for direct vendors.
    pub route_provider: Option<String>,
    pub display_order: usize,
    /// Set when this model's score was borrowed from a same-stem sibling
    /// because the ranking API has no entry for it yet. Holds the sibling's
    /// name; UI surfaces this so the fallback is visible.
    pub fallback_from: Option<String>,
}
/// Outcome of a dashboard refresh. ipbr is the sole authoritative source;
/// when the fetch succeeds the caller may persist the result. When it fails
/// `load_models_async` returns `Err`, and the caller is expected to keep
/// using the previously cached entries.
pub struct LoadOutcome {
    pub models: Vec<DashboardModel>,
    pub warnings: Vec<String>,
}
pub async fn load_models_async() -> Result<LoadOutcome> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;
    let scores = load_scores(&client).await?;
    let mut inventory = Vec::new();
    append_opencode_inventory(&mut inventory, opencode::enumerate_models());
    let merged = merge_with_warnings(inventory, scores);
    Ok(LoadOutcome {
        models: merged.models,
        warnings: merged.warnings,
    })
}
pub(crate) fn append_opencode_inventory(
    inventory: &mut Vec<InventoryEntry>,
    models: Vec<opencode::OpencodeModelMeta>,
) {
    inventory.extend(models.into_iter().filter_map(|meta| {
        // Both `opencode` (zen tier) and `opencode-go` (Go tier) ride the
        // same shared quota and surface as `vendor = "opencode"` in the UI;
        // they only diverge at ACP launch time, where route_provider picks
        // the qualifier. Other provider ids (openrouter, etc.) stay out.
        if meta.provider_id != "opencode" && meta.provider_id != "opencode-go" {
            return None;
        }
        let name = meta.id.trim().to_ascii_lowercase();
        if name.is_empty() {
            return None;
        }
        Some(InventoryEntry {
            name,
            vendor: "opencode".to_string(),
            route_underlying_vendor: meta.underlying_vendor,
            route_provider: Some(meta.provider_id),
        })
    }));
}
async fn load_scores(client: &Client) -> Result<Vec<ScoreEntry>> {
    let body = client
        .get(IPBR_SCOREBOARD_URL)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("ipbr scoreboard request failed")?
        .text()
        .await
        .context("ipbr scoreboard response body unreadable")?;
    parse_ipbr_scoreboard(&body)
}
/// TOML schema for the ipbr scoreboard. Unknown fields are ignored by
/// serde for forward compatibility per spec §"Error Handling".
#[derive(Debug, Deserialize, Default)]
struct IpbrScoreboard {
    #[serde(default)]
    models: Vec<IpbrModelRow>,
}
#[derive(Debug, Deserialize)]
struct IpbrModelRow {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    canonical_id: Option<String>,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    scores: Option<IpbrScoresRow>,
}
#[derive(Debug, Deserialize, Default)]
struct IpbrScoresRow {
    #[serde(default)]
    i_adj: Option<f64>,
    #[serde(default)]
    p_adj: Option<f64>,
    #[serde(default)]
    b_adj: Option<f64>,
    #[serde(default)]
    r: Option<f64>,
}
fn parse_ipbr_scoreboard(body: &str) -> Result<Vec<ScoreEntry>> {
    let board: IpbrScoreboard =
        toml::from_str(body).context("ipbr scoreboard was not valid TOML")?;
    let mut entries = Vec::new();
    for (i, row) in board.models.into_iter().enumerate() {
        // The merge key shape (lowercase only, no kebab/punctuation collapse)
        // matches the opencode inventory shape so the merge layer can cross
        // them with a normalized-exact lookup. Richer matching against the
        // richer `normalize_ipbr_key` form runs over `canonical_id` and
        // `aliases`.
        let display_key = row.display_name.trim().to_ascii_lowercase();
        if display_key.is_empty() {
            // No usable display_name: cannot index this row. Skip rather
            // than abort the whole feed; spec §"Error Handling" only
            // forces failure for malformed feed-level structure.
            continue;
        }
        let scores_row = row.scores.unwrap_or_default();
        let phase_scores = IpbrPhaseScores {
            idea: scores_row.i_adj,
            planning: scores_row.p_adj,
            build: scores_row.b_adj,
            review: scores_row.r,
        };
        let cosmetic = mean_present_phase_scores(&phase_scores).unwrap_or(0.0);
        let canonical_id = row
            .canonical_id
            .as_deref()
            .map(normalize_ipbr_key)
            .filter(|key| !key.is_empty());
        let aliases: Vec<String> = row
            .aliases
            .iter()
            .map(|alias| normalize_ipbr_key(alias))
            .filter(|key| !key.is_empty())
            .collect();
        entries.push(ScoreEntry {
            name: display_key,
            vendor: row.vendor.trim().to_ascii_lowercase(),
            // `overall_score` and `current_score` are cosmetic only —
            // selection MUST NOT use them as a phase-score fallback.
            overall_score: cosmetic,
            current_score: cosmetic,
            standard_error: 0.0,
            axes: Vec::new(),
            axis_provenance: BTreeMap::new(),
            display_order: i,
            canonical_id,
            aliases,
            ipbr_phase_scores: phase_scores,
            score_source: ScoreSource::Ipbr,
            // Every parsed ipbr row IS an ipbr-matched row; downstream
            // merging into inventory will preserve this flag when the row
            // attaches to an inventory model.
            ipbr_row_matched: true,
        });
    }
    anyhow::ensure!(!entries.is_empty(), "ipbr scoreboard returned no models");
    Ok(entries)
}
fn mean_present_phase_scores(scores: &IpbrPhaseScores) -> Option<f64> {
    let values: Vec<f64> = [scores.idea, scores.planning, scores.build, scores.review]
        .into_iter()
        .flatten()
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}
#[cfg(test)]
fn parse_dashboard_scores(payload: &Value) -> Result<Vec<ScoreEntry>> {
    use crate::data::dashboard_model::{value_to_f64, value_to_string};
    let data = payload.get("data").unwrap_or(payload);
    let model_scores = data
        .get("modelScores")
        .or_else(|| payload.get("modelScores"))
        .and_then(Value::as_array)
        .context("dashboard payload missing modelScores")?;
    let history_map = data
        .get("historyMap")
        .or_else(|| payload.get("historyMap"))
        .and_then(Value::as_object);
    let mut entries = Vec::new();
    for (i, item) in model_scores.iter().enumerate() {
        let name = item
            .get("name")
            .or_else(|| item.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let model_id = item.get("id").map(value_to_string).unwrap_or_default();
        let (axes, axis_provenance) = history_map
            .and_then(|map| map.get(&model_id))
            .and_then(merged_axes)
            .unwrap_or_default();
        entries.push(ScoreEntry {
            name,
            vendor: item
                .get("vendor")
                .or_else(|| item.get("provider"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase(),
            overall_score: value_to_f64(item.get("score")).unwrap_or(0.0),
            current_score: value_to_f64(item.get("currentScore"))
                .or_else(|| value_to_f64(item.get("score")))
                .unwrap_or(0.0),
            standard_error: value_to_f64(
                item.get("standardError")
                    .or_else(|| item.get("standard_error")),
            )
            .unwrap_or(0.0),
            axes,
            axis_provenance,
            display_order: i,
            canonical_id: None,
            aliases: Vec::new(),
            // Legacy aistupidlevel rows MUST NOT pretend to be ipbr
            // authority. Per spec, only ipbr provenance can drive
            // automatic phase selection.
            ipbr_phase_scores: IpbrPhaseScores::default(),
            score_source: ScoreSource::None,
            ipbr_row_matched: false,
        });
    }
    anyhow::ensure!(!entries.is_empty(), "dashboard returned no models");
    Ok(entries)
}
#[cfg(test)]
#[allow(clippy::type_complexity)]
fn merged_axes(value: &Value) -> Option<(Vec<(String, f64)>, BTreeMap<String, String>)> {
    let (axes, provenance, events) = crate::data::dashboard_model::merged_axes(value)?;
    for event in events {
        match event {
            IngestEvent::AxisDropped { reason } => record_axis_dropped(&reason),
            IngestEvent::AxisParseFail { suite, axis } => record_axis_parse_fail(&suite, &axis),
        }
    }
    Some((axes, provenance))
}
#[cfg(test)]
mod tests_mod;
