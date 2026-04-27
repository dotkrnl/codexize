use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use crate::model_names;

pub const MODELS_LIST_URL: &str = "https://aistupidlevel.info/api/models";
pub const DASHBOARD_URL: &str = "https://aistupidlevel.info/dashboard/cached";

#[derive(Debug, Clone)]
pub struct DashboardModel {
    pub name: String,
    pub vendor: String,
    pub overall_score: f64,
    pub current_score: f64,
    pub standard_error: f64,
    /// Values are 0.0..=1.0 floats from the aistupidlevel API; keys are
    /// lowercased camelCase. Backfill semantics are owned by the selection layer.
    pub axes: Vec<(String, f64)>,
    pub axis_provenance: BTreeMap<String, String>,
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
    axis_provenance: BTreeMap<String, String>,
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
        entries.push(InventoryEntry {
            name,
            vendor,
            display_order: i,
        });
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

    parse_dashboard_scores(&payload)
}

fn parse_dashboard_scores(payload: &Value) -> Result<Vec<ScoreEntry>> {
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
        });
    }

    anyhow::ensure!(!entries.is_empty(), "dashboard returned no models");
    Ok(entries)
}

// Merge inventory (full model list) with score data.
// Inventory drives the universe; scores enrich it where available.
fn merge(inventory: Vec<InventoryEntry>, scores: Vec<ScoreEntry>) -> Vec<DashboardModel> {
    let score_map: HashMap<String, &ScoreEntry> =
        scores.iter().map(|s| (s.name.clone(), s)).collect();

    // Also build a map keyed by score display_order for final sort
    let mut models: Vec<DashboardModel> = inventory
        .into_iter()
        .map(|inv| {
            if let Some(sc) = score_map.get(&inv.name) {
                DashboardModel {
                    name: inv.name,
                    vendor: if !inv.vendor.is_empty() {
                        inv.vendor
                    } else {
                        sc.vendor.clone()
                    },
                    overall_score: sc.overall_score,
                    current_score: sc.current_score,
                    standard_error: sc.standard_error,
                    axes: sc.axes.clone(),
                    axis_provenance: sc.axis_provenance.clone(),
                    display_order: sc.display_order,
                    fallback_from: None,
                }
            } else if let Some(sc) = sibling_score(&inv.name, &scores) {
                DashboardModel {
                    name: inv.name,
                    vendor: if !inv.vendor.is_empty() {
                        inv.vendor
                    } else {
                        sc.vendor.clone()
                    },
                    overall_score: sc.overall_score,
                    current_score: sc.current_score,
                    standard_error: sc.standard_error,
                    axes: sc.axes.clone(),
                    axis_provenance: sc.axis_provenance.clone(),
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
                    axis_provenance: BTreeMap::new(),
                    display_order: inv.display_order + 10_000,
                    fallback_from: None,
                }
            }
        })
        .collect();

    // Also add scored models not in the inventory (edge case)
    let inv_names: std::collections::HashSet<String> =
        models.iter().map(|m| m.name.clone()).collect();
    for sc in &scores {
        if !inv_names.contains(&sc.name) {
            models.push(DashboardModel {
                name: sc.name.clone(),
                vendor: sc.vendor.clone(),
                overall_score: sc.overall_score,
                current_score: sc.current_score,
                standard_error: sc.standard_error,
                axes: sc.axes.clone(),
                axis_provenance: sc.axis_provenance.clone(),
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
            axis_provenance: sc.axis_provenance,
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
            axis_provenance: BTreeMap::new(),
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
        vendor: if !vendor.is_empty() {
            vendor.to_string()
        } else {
            sibling.vendor.clone()
        },
        overall_score: sibling.overall_score,
        current_score: sibling.current_score,
        standard_error: sibling.standard_error,
        axes: sibling.axes.clone(),
        axis_provenance: sibling.axis_provenance.clone(),
        display_order: sibling.display_order,
        fallback_from: Some(sibling.name.clone()),
    })
}

fn explicit_fallback<'a>(name: &str, existing: &'a [DashboardModel]) -> Option<&'a DashboardModel> {
    let target = model_names::EXPLICIT_SCORE_FALLBACKS
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)?;
    existing.iter().find(|m| m.name == target)
}

/// Walk `historyMap[modelId]` newest-first, collecting the first numeric
/// value seen for each lowercased axis key.  Drops `contextwindow` and
/// skips unparseable values rather than coercing them to 0.0.
#[allow(clippy::type_complexity)]
fn merged_axes(value: &Value) -> Option<(Vec<(String, f64)>, BTreeMap<String, String>)> {
    let entries = value.as_array()?;
    let mut axes: BTreeMap<String, f64> = BTreeMap::new();
    let mut provenance: BTreeMap<String, String> = BTreeMap::new();

    for entry in entries.iter().rev() {
        let suite = entry
            .get("suite")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(entry_axes) = entry.get("axes").and_then(Value::as_object) else {
            continue;
        };
        for (k, v) in entry_axes {
            let key = k.to_ascii_lowercase();
            if key == "contextwindow" {
                provenance
                    .entry(key)
                    .or_insert_with(|| "dropped:contextwindow".to_string());
                eprintln!("codexize: ingest.axis_dropped reason=contextwindow");
                continue;
            }
            if axes.contains_key(&key) {
                continue;
            }
            match value_to_f64(Some(v)) {
                Some(num) => {
                    axes.insert(key.clone(), num);
                    provenance.insert(key, format!("suite:{suite}"));
                }
                None => {
                    eprintln!("codexize: ingest.axis_parse_fail suite={suite} axis={key}");
                }
            }
        }
    }

    Some((axes.into_iter().collect(), provenance))
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
    use crate::selection::config::SelectionPhase;
    use crate::selection::ranking::{build_version_index, selection_probability};
    use crate::selection::types::{CachedModel, VendorKind};

    fn model(name: &str, score: f64) -> DashboardModel {
        DashboardModel {
            name: name.to_string(),
            vendor: String::new(),
            overall_score: score,
            current_score: score,
            standard_error: 0.0,
            axes: Vec::new(),
            axis_provenance: BTreeMap::new(),
            display_order: 0,
            fallback_from: None,
        }
    }

    fn fixture_cached_models() -> Vec<CachedModel> {
        let payload: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/aistupidlevel_2026-04-26_subset.json"
        ))
        .expect("fixture should be valid JSON");
        parse_dashboard_scores(&payload)
            .expect("fixture should parse")
            .into_iter()
            .map(|entry| CachedModel {
                vendor: match entry.vendor.as_str() {
                    "anthropic" | "claude" => VendorKind::Claude,
                    "openai" => VendorKind::Codex,
                    "google" => VendorKind::Gemini,
                    "moonshotai" => VendorKind::Kimi,
                    other => panic!("fixture vendor {other} is not mapped"),
                },
                name: entry.name,
                overall_score: entry.overall_score,
                current_score: entry.current_score,
                standard_error: entry.standard_error,
                axes: entry.axes,
                axis_provenance: entry.axis_provenance,
                quota_percent: Some(80),
                display_order: entry.display_order,
                fallback_from: None,
            })
            .collect()
    }

    fn rounded_probability(value: f64) -> f64 {
        (value * 1_000_000.0).round() / 1_000_000.0
    }

    #[test]
    fn synthesize_gemini_3_1_pro_preview_falls_back_to_3_pro_preview() {
        let existing = vec![
            model("gemini-3-pro-preview", 80.0),
            model("gemini-2.5-pro", 70.0),
        ];
        let synth = synthesize_sibling("gemini-3.1-pro-preview", "google", &existing).unwrap();
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

    #[test]
    fn fixture_prechange_selection_probability_baseline_matches_artifact() {
        let models = fixture_cached_models();
        let version_index = build_version_index(&models);
        let phases = [
            ("idea", SelectionPhase::Idea),
            ("planning", SelectionPhase::Planning),
            ("build", SelectionPhase::Build),
            ("review", SelectionPhase::Review),
        ];
        let mut snapshot = BTreeMap::new();
        for model in &models {
            let mut phase_probabilities = BTreeMap::new();
            for (label, phase) in phases {
                phase_probabilities.insert(
                    label.to_string(),
                    rounded_probability(selection_probability(model, phase, &version_index)),
                );
            }
            snapshot.insert(model.name.clone(), phase_probabilities);
        }

        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/aistupidlevel_2026-04-26_prechange_selection_probabilities.json"
        ))
        .expect("baseline artifact should be valid JSON");
        let actual = serde_json::to_value(snapshot).unwrap();
        assert_eq!(actual, expected);
    }

    fn fixture_score_entries() -> Vec<ScoreEntry> {
        let payload: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/aistupidlevel_2026-04-26_subset.json"
        ))
        .expect("fixture should be valid JSON");
        parse_dashboard_scores(&payload).expect("fixture should parse")
    }

    #[test]
    fn merged_axes_populates_tooling_axes_for_models_with_tooling_history() {
        let entries = fixture_score_entries();
        for entry in &entries {
            let has_tooling = entry.axis_provenance.values().any(|v| v == "suite:tooling");
            if has_tooling {
                assert!(
                    entry.axes.iter().any(|(k, _)| k == "contextawareness"),
                    "{} should have contextawareness",
                    entry.name
                );
                assert!(
                    entry.axes.iter().any(|(k, _)| k == "taskcompletion"),
                    "{} should have taskcompletion",
                    entry.name
                );
            }
        }
        let glm46 = entries.iter().find(|e| e.name == "glm-4.6").unwrap();
        assert!(
            glm46.axis_provenance.values().any(|v| v == "suite:tooling"),
            "glm-4.6 should have tooling provenance"
        );
    }

    #[test]
    fn merged_axes_drops_contextwindow_with_provenance() {
        let entries = fixture_score_entries();
        for entry in &entries {
            assert!(
                !entry.axes.iter().any(|(k, _)| k == "contextwindow"),
                "{} should not have contextwindow in axes",
                entry.name
            );
        }
        // Models whose upstream payload contained contextWindow should have the drop label
        let glm46 = entries.iter().find(|e| e.name == "glm-4.6").unwrap();
        assert_eq!(
            glm46
                .axis_provenance
                .get("contextwindow")
                .map(String::as_str),
            Some("dropped:contextwindow")
        );
        let gemini = entries.iter().find(|e| e.name == "gemini-2.5-pro").unwrap();
        assert_eq!(
            gemini
                .axis_provenance
                .get("contextwindow")
                .map(String::as_str),
            Some("dropped:contextwindow")
        );
    }

    #[test]
    fn merged_axes_skips_parse_failure_without_coercing_to_zero() {
        let entries = fixture_score_entries();
        let gemini = entries.iter().find(|e| e.name == "gemini-2.5-pro").unwrap();
        // The fixture has correctness: "parse-failure" for gemini's deep suite.
        // It should be absent from axes (not present as 0.0).
        assert!(
            !gemini.axes.iter().any(|(k, _)| k == "correctness"),
            "correctness should be absent (parse failure), not coerced to 0.0"
        );
    }

    #[test]
    fn merged_axes_newest_first_preserves_newer_values() {
        // For GLM models (id 165, 217), the fixture has tooling first (older)
        // then deep (newer, last in array). Both have "efficiency".
        // Newest-first means deep's efficiency wins over tooling's.
        let entries = fixture_score_entries();
        let glm46 = entries.iter().find(|e| e.name == "glm-4.6").unwrap();
        let efficiency = glm46
            .axes
            .iter()
            .find(|(k, _)| k == "efficiency")
            .map(|(_, v)| *v);
        // deep entry has efficiency=0.42, tooling has efficiency=0.0
        assert_eq!(efficiency, Some(0.42));
        assert_eq!(
            glm46.axis_provenance.get("efficiency").map(String::as_str),
            Some("suite:deep")
        );
    }

    #[test]
    fn merged_axes_provenance_labels_match_contract() {
        let entries = fixture_score_entries();
        for entry in &entries {
            for (axis, label) in &entry.axis_provenance {
                assert!(
                    label.starts_with("suite:")
                        || label.starts_with("dropped:")
                        || label.starts_with("fallback:"),
                    "{}: axis '{}' has unexpected provenance label '{}'",
                    entry.name,
                    axis,
                    label
                );
            }
        }
    }

    #[test]
    fn merged_axes_handles_empty_history() {
        let val = serde_json::json!([]);
        let result = merged_axes(&val);
        assert!(result.is_some());
        let (axes, prov) = result.unwrap();
        assert!(axes.is_empty());
        assert!(prov.is_empty());
    }

    #[test]
    fn merged_axes_handles_missing_suite_tag() {
        let val = serde_json::json!([
            {
                "axes": { "correctness": 0.9 }
            }
        ]);
        let (axes, prov) = merged_axes(&val).unwrap();
        assert_eq!(axes.len(), 1);
        assert_eq!(axes[0], ("correctness".to_string(), 0.9));
        // Empty suite tag → "suite:"
        assert_eq!(prov.get("correctness").map(String::as_str), Some("suite:"));
    }

    #[test]
    fn ingest_axis_dropped_counter_fires_for_contextwindow() {
        // Verify the counter string appears in stderr by calling merged_axes
        // on input containing contextWindow. The counter is an eprintln! so
        // we verify it via the provenance label as a proxy.
        let val = serde_json::json!([
            {
                "suite": "deep",
                "axes": { "contextWindow": 0.0, "correctness": 0.5 }
            }
        ]);
        let (axes, prov) = merged_axes(&val).unwrap();
        assert!(
            !axes.iter().any(|(k, _)| k == "contextwindow"),
            "contextwindow should be dropped from axes"
        );
        assert_eq!(
            prov.get("contextwindow").map(String::as_str),
            Some("dropped:contextwindow")
        );
    }

    #[test]
    fn ingest_axis_parse_fail_counter_fires_for_bad_values() {
        let val = serde_json::json!([
            {
                "suite": "deep",
                "axes": { "correctness": "not-a-number" }
            }
        ]);
        let (axes, prov) = merged_axes(&val).unwrap();
        assert!(
            !axes.iter().any(|(k, _)| k == "correctness"),
            "unparseable axis should not appear in axes"
        );
        assert!(
            !prov.contains_key("correctness"),
            "unparseable axis should not have provenance"
        );
    }
}
