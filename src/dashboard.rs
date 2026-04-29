use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub use crate::dashboard_view_model::synthesize_sibling;
use crate::dashboard_view_model::{
    InventoryEntry, ScoreEntry, inv_only, merge, scores_only, value_to_f64, value_to_string,
};

/// Counter events emitted by ingestion. Production callers stream them
/// to stderr; tests read the in-memory log to assert labels without a
/// subprocess capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestEvent {
    AxisDropped { reason: String },
    AxisParseFail { suite: String, axis: String },
}

fn ingest_events() -> &'static Mutex<Vec<IngestEvent>> {
    static EVENTS: OnceLock<Mutex<Vec<IngestEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Snapshot of every ingest event recorded since process start (or since
/// the last `clear_ingest_events`). Intended for test assertions.
pub fn ingest_events_snapshot() -> Vec<IngestEvent> {
    // SAFETY: `ingest_events()` guards a `Vec<IngestEvent>` whose only
    // mutators are `push`/`clear` — neither can panic — so the mutex
    // poison branch is only defensive for future mutators.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

#[cfg(test)]
fn clear_ingest_events() {
    // SAFETY: see `ingest_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
}

fn record_axis_dropped(reason: &str) {
    // SAFETY: see `ingest_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(IngestEvent::AxisDropped {
            reason: reason.to_string(),
        });
}

fn record_axis_parse_fail(suite: &str, axis: &str) {
    // SAFETY: see `ingest_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    ingest_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(IngestEvent::AxisParseFail {
            suite: suite.to_string(),
            axis: axis.to_string(),
        });
}

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

#[allow(clippy::type_complexity)]
fn merged_axes(value: &Value) -> Option<(Vec<(String, f64)>, BTreeMap<String, String>)> {
    let (axes, provenance, events) = crate::dashboard_view_model::merged_axes(value)?;
    for event in events {
        match event {
            IngestEvent::AxisDropped { reason } => record_axis_dropped(&reason),
            IngestEvent::AxisParseFail { suite, axis } => record_axis_parse_fail(&suite, &axis),
        }
    }
    Some((axes, provenance))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::config::SelectionPhase;
    use crate::selection::ranking::{
        build_version_index, selection_probability, stamp_selection_provenance,
    };
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

    fn fixture_postchange_snapshot() -> BTreeMap<String, BTreeMap<String, f64>> {
        let mut models = fixture_cached_models();
        for model in &mut models {
            stamp_selection_provenance(model);
        }
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
        snapshot
    }

    #[test]
    fn fixture_postchange_selection_probability_matches_artifact() {
        let snapshot = fixture_postchange_snapshot();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/aistupidlevel_2026-04-26_postchange_selection_probabilities.json"
        ))
        .expect("post-change artifact should be valid JSON");
        let actual = serde_json::to_value(snapshot).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn glm46_clears_inclusion_gate_when_newest_glm_in_pool() {
        let filtered_model_set: &[&str] =
            &["claude-sonnet-4.6", "gemini-2.5-pro", "glm-4.6", "gpt-5.4"];
        const RATIO: f64 = 1.0 / 3.0;
        const COMPARISON_ANCHOR_MODEL: &str = "gpt-5.4";
        const COMPARISON_ANCHOR_BUILD: f64 = 0.807161; // gpt-5.4 Build, post-change
        let mut models = fixture_cached_models();
        models.retain(|model| filtered_model_set.contains(&model.name.as_str()));
        for model in &mut models {
            stamp_selection_provenance(model);
        }
        let actual: std::collections::BTreeSet<&str> =
            models.iter().map(|m| m.name.as_str()).collect();
        let expected: std::collections::BTreeSet<&str> =
            filtered_model_set.iter().copied().collect();
        assert_eq!(actual, expected, "filtered fixture model set drifted");
        let version_index = build_version_index(&models);
        let anchor = models
            .iter()
            .find(|m| m.name == COMPARISON_ANCHOR_MODEL)
            .unwrap();
        let anchor_prob = rounded_probability(selection_probability(
            anchor,
            SelectionPhase::Build,
            &version_index,
        ));
        assert_eq!(
            anchor_prob, COMPARISON_ANCHOR_BUILD,
            "{COMPARISON_ANCHOR_MODEL} Build anchor drifted"
        );
        let glm46 = models.iter().find(|m| m.name == "glm-4.6").unwrap();
        let glm46_prob = selection_probability(glm46, SelectionPhase::Build, &version_index);
        let cutoff = COMPARISON_ANCHOR_BUILD * RATIO;
        assert!(
            glm46_prob > cutoff,
            "glm-4.6 Build ({glm46_prob}) should exceed cutoff ({cutoff}) = {COMPARISON_ANCHOR_MODEL} Build {COMPARISON_ANCHOR_BUILD} * {RATIO}"
        );
    }

    #[test]
    fn glm46_post_change_substantially_improved() {
        // AC #4a-bis: the unfiltered fixture still includes glm-4.7, so
        // this asserts the pinned lift while leaving the version penalty
        // unchanged as required by spec §4.2 / §8.
        let prechange: BTreeMap<String, BTreeMap<String, f64>> =
            serde_json::from_str(include_str!(
                "../tests/fixtures/aistupidlevel_2026-04-26_prechange_selection_probabilities.json"
            ))
            .unwrap();
        let postchange = fixture_postchange_snapshot();
        let pre = prechange["glm-4.6"]["build"];
        let post = postchange["glm-4.6"]["build"];
        assert!(
            post > pre * 30.0,
            "post-change glm-4.6 Build ({post}) should be ≥30× pre-change ({pre})"
        );
    }

    #[test]
    fn prechange_selectable_models_remain_above_gate() {
        // AC #4b: explicit pre-change selectable set captured from task #1
        let prechange_selectable: &[&str] = &["claude-sonnet-4.6", "gemini-2.5-pro", "gpt-5.4"];
        let mut models = fixture_cached_models();
        for model in &mut models {
            stamp_selection_provenance(model);
        }
        let version_index = build_version_index(&models);
        let ratio = 1.0 / 3.0;

        for phase in [
            SelectionPhase::Build,
            SelectionPhase::Planning,
            SelectionPhase::Review,
        ] {
            let probs: Vec<(&str, f64)> = models
                .iter()
                .map(|m| {
                    (
                        m.name.as_str(),
                        selection_probability(m, phase, &version_index),
                    )
                })
                .collect();
            let max_prob = probs.iter().map(|(_, p)| *p).fold(0.0_f64, f64::max);
            let cutoff = max_prob * ratio;
            for &name in prechange_selectable {
                let prob = probs.iter().find(|(n, _)| *n == name).unwrap().1;
                assert!(
                    prob >= cutoff,
                    "{name} in {:?} ({prob}) fell below cutoff ({cutoff})",
                    phase.name()
                );
            }
        }
    }

    #[test]
    fn ranking_order_among_healthy_models_unchanged() {
        // AC #8: healthy models (no zero axis) keep the same ranking order.
        // Healthy models identified from fixture axes (not derived dynamically):
        // - claude-sonnet-4.6: hourly suite, all axes non-zero
        // - gemini-2.5-pro: parse-failure on correctness (skipped), contextWindow dropped, rest non-zero
        // - gpt-5.4: hourly suite, all axes non-zero
        // Unhealthy (have zero axes): glm-4.6, glm-4.7
        let healthy: &[&str] = &["claude-sonnet-4.6", "gemini-2.5-pro", "gpt-5.4"];

        let prechange: BTreeMap<String, BTreeMap<String, f64>> =
            serde_json::from_str(include_str!(
                "../tests/fixtures/aistupidlevel_2026-04-26_prechange_selection_probabilities.json"
            ))
            .unwrap();
        let postchange = fixture_postchange_snapshot();

        for phase_name in ["build", "idea", "planning", "review"] {
            let mut pre_order: Vec<(&str, f64)> = healthy
                .iter()
                .map(|&name| (name, prechange[name][phase_name]))
                .collect();
            pre_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            let mut post_order: Vec<(&str, f64)> = healthy
                .iter()
                .map(|&name| (name, postchange[name][phase_name]))
                .collect();
            post_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            let pre_names: Vec<&str> = pre_order.iter().map(|(n, _)| *n).collect();
            let post_names: Vec<&str> = post_order.iter().map(|(n, _)| *n).collect();
            assert_eq!(
                pre_names, post_names,
                "ranking order changed in {phase_name}: pre={pre_names:?} post={post_names:?}"
            );
        }
    }

    #[test]
    fn idea_phase_uses_contextawareness_and_taskcompletion() {
        let mut models = fixture_cached_models();
        for model in &mut models {
            stamp_selection_provenance(model);
        }
        // Pick a model that has contextawareness and taskcompletion from
        // tooling suite (glm-4.6 or glm-4.7 in the fixture)
        let glm = models.iter().find(|m| m.name == "glm-4.6").unwrap();
        assert!(
            glm.axis("contextawareness").is_some(),
            "fixture model should have contextawareness"
        );
        assert!(
            glm.axis("taskcompletion").is_some(),
            "fixture model should have taskcompletion"
        );

        let index = build_version_index(&models);
        let score_with = selection_probability(glm, SelectionPhase::Idea, &index);

        // Synthetic variant with contextawareness and taskcompletion removed
        let mut stripped = glm.clone();
        stripped
            .axes
            .retain(|(k, _)| k != "contextawareness" && k != "taskcompletion");
        let score_without = selection_probability(&stripped, SelectionPhase::Idea, &index);

        assert!(
            (score_with - score_without).abs() > 1e-6,
            "Idea score should differ when contextawareness/taskcompletion present ({score_with}) vs absent ({score_without})"
        );
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

    /// Serializes the two ingest-counter tests so the in-memory event log
    /// can be cleared and inspected without races from parallel tests.
    fn ingest_counter_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn ingest_axis_dropped_counter_fires_for_contextwindow() {
        let _guard = ingest_counter_lock();
        clear_ingest_events();
        let val = serde_json::json!([
            {
                "suite": "deep",
                "axes": { "contextWindow": 0.0, "correctness": 0.5 }
            }
        ]);
        let (axes, prov) = merged_axes(&val).unwrap();
        assert!(!axes.iter().any(|(k, _)| k == "contextwindow"));
        assert_eq!(
            prov.get("contextwindow").map(String::as_str),
            Some("dropped:contextwindow")
        );
        let events = ingest_events_snapshot();
        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::AxisDropped { reason } if reason == "contextwindow"
            )),
            "expected ingest.axis_dropped reason=contextwindow, got {events:?}"
        );
    }

    #[test]
    fn ingest_axis_parse_fail_counter_fires_for_bad_values() {
        let _guard = ingest_counter_lock();
        clear_ingest_events();
        let val = serde_json::json!([
            {
                "suite": "deep",
                "axes": { "correctness": "not-a-number" }
            }
        ]);
        let (axes, prov) = merged_axes(&val).unwrap();
        assert!(!axes.iter().any(|(k, _)| k == "correctness"));
        assert!(!prov.contains_key("correctness"));
        let events = ingest_events_snapshot();
        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::AxisParseFail { suite, axis }
                    if suite == "deep" && axis == "correctness"
            )),
            "expected ingest.axis_parse_fail suite=deep axis=correctness, got {events:?}"
        );
    }

    #[test]
    fn fixture_emits_parse_fail_for_synthetic_row() {
        // The fixture's gemini-2.5-pro deep suite has correctness:"parse-failure".
        // Ingesting it must emit ingest.axis_parse_fail suite=deep axis=correctness
        // and leave correctness absent from the axes vector.
        let _guard = ingest_counter_lock();
        clear_ingest_events();
        let entries = fixture_score_entries();
        let gemini = entries.iter().find(|e| e.name == "gemini-2.5-pro").unwrap();
        assert!(!gemini.axes.iter().any(|(k, _)| k == "correctness"));
        let events = ingest_events_snapshot();
        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::AxisParseFail { suite, axis }
                    if suite == "deep" && axis == "correctness"
            )),
            "expected parse_fail event for gemini-2.5-pro deep correctness, got {events:?}"
        );
    }

    #[test]
    fn fixture_emits_axis_dropped_for_contextwindow() {
        let _guard = ingest_counter_lock();
        clear_ingest_events();
        let _ = fixture_score_entries();
        let events = ingest_events_snapshot();
        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::AxisDropped { reason } if reason == "contextwindow"
            )),
            "expected at least one axis_dropped reason=contextwindow event, got {events:?}"
        );
    }
}
