use crate::data::dashboard_model::{ScoreEntry, models_from_scores};
use crate::selection::{IpbrPhaseScores, ScoreSource};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
/// Source of truth for the model universe and its per-phase rank scores.
/// ipbr is the only upstream feed; provider entries are the launch inventory.
pub const IPBR_SCOREBOARD_URL: &str = "https://ipbr.dev/scoreboard.toml";
#[derive(Debug, Clone)]
pub struct DashboardModel {
    pub name: String,
    pub dashboard_vendor: String,
    /// Per-phase ipbr rank scores. `None` per phase means the matched
    /// ipbr row did not provide that phase score.
    pub ipbr_phase_scores: crate::selection::IpbrPhaseScores,
    /// Where the per-phase rank scores came from. Defaults to
    /// `ScoreSource::None`; only `Ipbr` may drive automatic selection.
    pub score_source: crate::selection::ScoreSource,
    /// `true` when this model matched an ipbr row by normalized exact
    /// key. Inventory-/CLI-only visible models keep this `false`.
    pub ipbr_row_matched: bool,
    pub ipbr_match_key: Option<String>,
    pub display_order: usize,
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
    let merged = models_from_scores(scores);
    Ok(LoadOutcome {
        models: merged.models,
        warnings: merged.warnings,
    })
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
    vendor: String,
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
        // `display_name` is the canonical model id. Lowercase it for
        // case-stable provider matching, but do not collapse punctuation:
        // `claude-opus-4.6` and `claude-opus-4-6` are different ids.
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
        entries.push(ScoreEntry {
            name: display_key,
            vendor: row.vendor.trim().to_ascii_lowercase(),
            display_order: i,
            ipbr_phase_scores: phase_scores,
            score_source: ScoreSource::Ipbr,
            ipbr_row_matched: true,
        });
    }
    anyhow::ensure!(!entries.is_empty(), "ipbr scoreboard returned no models");
    Ok(entries)
}
#[cfg(test)]
mod tests_mod;
