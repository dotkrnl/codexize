use crate::data::dashboard_model::{ScoreEntry, models_from_scores};
use crate::selection::{IpbrStageScores, ScoreSource};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
/// Source of truth for the model universe and its per-stage rank scores.
/// ipbr is the only upstream feed; provider entries are the launch inventory.
pub const IPBR_SCOREBOARD_URL: &str = "https://ipbr.dev/scoreboard.toml";
#[derive(Debug, Clone)]
pub struct DashboardModel {
    pub name: String,
    /// Per-stage ipbr rank scores. `None` per stage means the matched
    /// ipbr row did not provide that stage score.
    pub ipbr_stage_scores: crate::selection::IpbrStageScores,
    /// Where the per-stage rank scores came from. Defaults to
    /// `ScoreSource::None`; only `Ipbr` may drive automatic selection.
    pub score_source: crate::selection::ScoreSource,
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
/// TOML fields codexize consumes from the ipbr scoreboard.
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
        // `example-1.0` and `example-1-0` are different ids.
        let display_key = row.display_name.trim().to_ascii_lowercase();
        if display_key.is_empty() {
            // No usable display_name: cannot index this row.
            continue;
        }
        let scores_row = row.scores.unwrap_or_default();
        let stage_scores = IpbrStageScores {
            idea: scores_row.i_adj,
            planning: scores_row.p_adj,
            build: scores_row.b_adj,
            review: scores_row.r,
        };
        entries.push(ScoreEntry {
            name: display_key,
            display_order: i,
            ipbr_stage_scores: stage_scores,
            score_source: ScoreSource::Ipbr,
        });
    }
    anyhow::ensure!(!entries.is_empty(), "ipbr scoreboard returned no models");
    Ok(entries)
}
#[cfg(test)]
mod tests_mod;
