use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub const MODELS_LIST_URL: &str = "https://aistupidlevel.info/api/models";
pub const DASHBOARD_URL: &str = "https://aistupidlevel.info/dashboard/cached";


#[derive(Debug, Clone)]
pub struct DashboardModel {
    pub name: String,
    pub vendor: String,
    pub overall_score: f64,
    pub current_score: f64,
    pub standard_error: f64,
    pub axes: Vec<(String, f64)>,
    pub display_order: usize,
    /// Set when this model's score was borrowed from a same-stem sibling
    /// because the ranking API has no entry for it yet. Holds the sibling's
    /// name; UI surfaces this so the fallback is visible.
    pub fallback_from: Option<String>,
}

pub fn load_models() -> Result<Vec<DashboardModel>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;

    // Load both sources in parallel via two requests
    let inventory = load_inventory(&client);
    let scores = load_scores(&client);

    match (inventory, scores) {
        (Ok(inv), Ok(sc)) => Ok(merge(inv, sc)),
        (Ok(inv), Err(_)) => Ok(inv_only(inv)),
        (Err(_), Ok(sc)) => Ok(scores_only(sc)),
        (Err(e1), Err(e2)) => {
            anyhow::bail!("both sources failed: inventory={e1}, dashboard={e2}")
        }
    }
}

// A lightweight model entry from the /api/models inventory
struct InventoryEntry {
    name: String,
    vendor: String,
    display_order: usize,
}

// A model entry from the dashboard with score data
struct ScoreEntry {
    name: String,
    vendor: String,
    overall_score: f64,
    current_score: f64,
    standard_error: f64,
    axes: Vec<(String, f64)>,
    display_order: usize,
}

fn load_inventory(client: &Client) -> Result<Vec<InventoryEntry>> {
    let payload = client
        .get(MODELS_LIST_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .context("models list request failed")?
        .json::<Value>()
        .context("models list was not valid JSON")?;

    let arr = payload
        .as_array()
        .context("models list is not a JSON array")?;

    let mut entries = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let vendor = item
            .get("vendor")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        entries.push(InventoryEntry { name, vendor, display_order: i });
    }

    anyhow::ensure!(!entries.is_empty(), "models list returned no entries");
    Ok(entries)
}

fn load_scores(client: &Client) -> Result<Vec<ScoreEntry>> {
    let payload = client
        .get(DASHBOARD_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .context("dashboard request failed")?
        .json::<Value>()
        .context("dashboard response was not valid JSON")?;

    let data = payload.get("data").unwrap_or(&payload);
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
        let axes = history_map
            .and_then(|map| map.get(&model_id))
            .and_then(latest_axes)
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
                item.get("standardError").or_else(|| item.get("standard_error")),
            )
            .unwrap_or(0.0),
            axes,
            display_order: i,
        });
    }

    anyhow::ensure!(!entries.is_empty(), "dashboard returned no models");
    Ok(entries)
}

// Merge inventory (full model list) with score data.
// Inventory drives the universe; scores enrich it where available.
fn merge(inventory: Vec<InventoryEntry>, scores: Vec<ScoreEntry>) -> Vec<DashboardModel> {
    let score_map: HashMap<String, &ScoreEntry> = scores
        .iter()
        .map(|s| (s.name.clone(), s))
        .collect();

    // Also build a map keyed by score display_order for final sort
    let mut models: Vec<DashboardModel> = inventory
        .into_iter()
        .map(|inv| {
            if let Some(sc) = score_map.get(&inv.name) {
                DashboardModel {
                    name: inv.name,
                    vendor: if !inv.vendor.is_empty() { inv.vendor } else { sc.vendor.clone() },
                    overall_score: sc.overall_score,
                    current_score: sc.current_score,
                    standard_error: sc.standard_error,
                    axes: sc.axes.clone(),
                    display_order: sc.display_order,
                    fallback_from: None,
                }
            } else if let Some(sc) = sibling_score(&inv.name, &scores) {
                DashboardModel {
                    name: inv.name,
                    vendor: if !inv.vendor.is_empty() { inv.vendor } else { sc.vendor.clone() },
                    overall_score: sc.overall_score,
                    current_score: sc.current_score,
                    standard_error: sc.standard_error,
                    axes: sc.axes.clone(),
                    display_order: sc.display_order,
                    fallback_from: Some(sc.name.clone()),
                }
            } else {
                // Model is in the inventory but not yet scored — include with zeroed scores
                DashboardModel {
                    name: inv.name,
                    vendor: inv.vendor,
                    overall_score: 0.0,
                    current_score: 0.0,
                    standard_error: 0.0,
                    axes: Vec::new(),
                    display_order: inv.display_order + 10_000,
                    fallback_from: None,
                }
            }
        })
        .collect();

    // Also add scored models not in the inventory (edge case)
    let inv_names: std::collections::HashSet<String> = models
        .iter()
        .map(|m| m.name.clone())
        .collect();
    for sc in &scores {
        if !inv_names.contains(&sc.name) {
            models.push(DashboardModel {
                name: sc.name.clone(),
                vendor: sc.vendor.clone(),
                overall_score: sc.overall_score,
                current_score: sc.current_score,
                standard_error: sc.standard_error,
                axes: sc.axes.clone(),
                display_order: sc.display_order,
                fallback_from: None,
            });
        }
    }

    models.sort_by_key(|m| m.display_order);
    models
}

// Fallback when inventory is unavailable: scores only
fn scores_only(scores: Vec<ScoreEntry>) -> Vec<DashboardModel> {
    scores
        .into_iter()
        .map(|sc| DashboardModel {
            name: sc.name,
            vendor: sc.vendor,
            overall_score: sc.overall_score,
            current_score: sc.current_score,
            standard_error: sc.standard_error,
            axes: sc.axes,
            display_order: sc.display_order,
            fallback_from: None,
        })
        .collect()
}

// Fallback when scores are unavailable: inventory only, zeroed scores
fn inv_only(inventory: Vec<InventoryEntry>) -> Vec<DashboardModel> {
    inventory
        .into_iter()
        .map(|inv| DashboardModel {
            name: inv.name,
            vendor: inv.vendor,
            overall_score: 0.0,
            current_score: 0.0,
            standard_error: 0.0,
            axes: Vec::new(),
            display_order: inv.display_order,
            fallback_from: None,
        })
        .collect()
}

// Strip a trailing point-version segment (e.g. "gpt-5.5" -> "gpt-5",
// "claude-sonnet-4.6" -> "claude-sonnet-4"). Returns None when the last
// `.`-separated segment isn't purely numeric, so we don't fall back across
// major versions.
fn version_stem(name: &str) -> Option<&str> {
    let (prefix, tail) = name.rsplit_once('.')?;
    if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
        Some(prefix)
    } else {
        None
    }
}

// Find the best-scoring sibling with the same version stem — e.g. use
// gpt-5.4's score for gpt-5.5 until the ranking API catches up. Returns
// None when there is no sibling (no fallback applied).
fn sibling_score<'a>(name: &str, scores: &'a [ScoreEntry]) -> Option<&'a ScoreEntry> {
    let stem = version_stem(name)?;
    scores
        .iter()
        .filter(|sc| sc.name != name && version_stem(&sc.name) == Some(stem))
        .max_by(|a, b| {
            a.overall_score
                .partial_cmp(&b.overall_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

// Hardcoded fallbacks for cases the same-stem heuristic can't express
// (cross-major-version or non-numeric suffix differences). Listed in
// preference order; first match wins. Used when the ranking API hasn't
// scored the new model yet but quota probes already see it.
const EXPLICIT_FALLBACKS: &[(&str, &str)] = &[
    ("gemini-3.1-pro-preview", "gemini-3-pro-preview"),
    ("gemini-3-flash-preview", "gemini-2.5-flash"),
];

// Synthesize a DashboardModel for a name absent from the ranking API by
// borrowing a sibling's numbers — preferring an explicit hardcoded mapping
// (e.g. gemini-3-flash-preview → gemini-2.5-flash), then falling back to the
// best-scoring same-stem sibling. Used by the selection layer to keep
// live-quota-only models in the candidate pool. The synthesized model
// carries `fallback_from = Some(sibling.name)` so the UI surfaces the
// fallback only for models that actually survive selection; once the real
// score lands in `existing`, the caller finds an exact match and never
// calls this.
pub fn synthesize_sibling(
    name: &str,
    vendor: &str,
    existing: &[DashboardModel],
) -> Option<DashboardModel> {
    let sibling = explicit_fallback(name, existing).or_else(|| {
        let stem = version_stem(name)?;
        existing
            .iter()
            .filter(|m| m.name != name && version_stem(&m.name) == Some(stem))
            .max_by(|a, b| {
                a.overall_score
                    .partial_cmp(&b.overall_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    })?;

    Some(DashboardModel {
        name: name.to_string(),
        vendor: if !vendor.is_empty() { vendor.to_string() } else { sibling.vendor.clone() },
        overall_score: sibling.overall_score,
        current_score: sibling.current_score,
        standard_error: sibling.standard_error,
        axes: sibling.axes.clone(),
        display_order: sibling.display_order,
        fallback_from: Some(sibling.name.clone()),
    })
}

fn explicit_fallback<'a>(name: &str, existing: &'a [DashboardModel]) -> Option<&'a DashboardModel> {
    let target = EXPLICIT_FALLBACKS
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)?;
    existing.iter().find(|m| m.name == target)
}

fn latest_axes(value: &Value) -> Option<Vec<(String, f64)>> {
    let latest = value.as_array()?.last()?;
    let axes = latest.get("axes")?.as_object()?;
    Some(
        axes.iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), value_to_f64(Some(v)).unwrap_or(0.0)))
            .collect(),
    )
}

fn value_to_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str, score: f64) -> DashboardModel {
        DashboardModel {
            name: name.to_string(),
            vendor: String::new(),
            overall_score: score,
            current_score: score,
            standard_error: 0.0,
            axes: Vec::new(),
            display_order: 0,
            fallback_from: None,
        }
    }

    #[test]
    fn synthesize_gemini_3_1_pro_preview_falls_back_to_3_pro_preview() {
        let existing = vec![
            model("gemini-3-pro-preview", 80.0),
            model("gemini-2.5-pro", 70.0),
        ];
        let synth =
            synthesize_sibling("gemini-3.1-pro-preview", "google", &existing).unwrap();
        assert_eq!(synth.fallback_from.as_deref(), Some("gemini-3-pro-preview"));
        assert_eq!(synth.overall_score, 80.0);
    }

    #[test]
    fn synthesize_gemini_3_flash_preview_falls_back_to_2_5_flash() {
        let existing = vec![
            model("gemini-2.5-flash", 60.0),
            model("gemini-2.5-pro", 75.0),
        ];
        let synth = synthesize_sibling("gemini-3-flash-preview", "google", &existing).unwrap();
        assert_eq!(synth.fallback_from.as_deref(), Some("gemini-2.5-flash"));
        assert_eq!(synth.overall_score, 60.0);
    }

    #[test]
    fn synthesize_gpt_5_5_uses_same_stem_sibling() {
        let existing = vec![model("gpt-5.2", 70.0), model("gpt-4.1", 50.0)];
        let synth = synthesize_sibling("gpt-5.5", "openai", &existing).unwrap();
        assert_eq!(synth.fallback_from.as_deref(), Some("gpt-5.2"));
    }
}
