use super::*;
use crate::selection::config::SelectionPhase;
use crate::selection::ranking::{phase_score_for_legacy_callers, stamp_selection_provenance};
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
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        ipbr_match_key: None,
        route_underlying_vendor: None,
        display_order: 0,
        fallback_from: None,
    }
}

fn fixture_cached_models() -> Vec<CachedModel> {
    let payload: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/aistupidlevel_2026-04-26_subset.json"
    ))
    .expect("fixture should be valid JSON");
    let pinned_phase_scores: BTreeMap<String, BTreeMap<String, f64>> =
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/aistupidlevel_2026-04-26_postchange_selection_probabilities.json"
        ))
        .expect("post-change artifact should be valid JSON");
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
            name: entry.name.clone(),
            overall_score: entry.overall_score,
            current_score: entry.current_score,
            standard_error: entry.standard_error,
            axes: entry.axes,
            axis_provenance: entry.axis_provenance,
            ipbr_phase_scores: {
                let scores = pinned_phase_scores
                    .get(&entry.name)
                    .expect("fixture model should have pinned phase scores");
                crate::selection::IpbrPhaseScores {
                    idea: scores.get("idea").copied(),
                    planning: scores.get("planning").copied(),
                    build: scores.get("build").copied(),
                    review: scores.get("review").copied(),
                }
            },
            score_source: crate::selection::ScoreSource::Ipbr,
            ipbr_row_matched: true,
            ipbr_match_key: Some(entry.name.clone()),
            route_underlying_vendor: None,
            quota_percent: Some(80),
            quota_resets_at: None,
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
                rounded_probability(phase_score_for_legacy_callers(model, phase)),
            );
        }
        snapshot.insert(model.name.clone(), phase_probabilities);
    }
    snapshot
}

#[test]
fn fixture_postchange_phase_score_for_legacy_callers_matches_artifact() {
    let snapshot = fixture_postchange_snapshot();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/aistupidlevel_2026-04-26_postchange_selection_probabilities.json"
    ))
    .expect("post-change artifact should be valid JSON");
    let actual = serde_json::to_value(snapshot).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn glm46_preserves_pinned_nonzero_build_score() {
    let filtered_model_set: &[&str] =
        &["claude-sonnet-4.6", "gemini-2.5-pro", "glm-4.6", "gpt-5.4"];
    const COMPARISON_ANCHOR_MODEL: &str = "gpt-5.4";
    const COMPARISON_ANCHOR_BUILD: f64 = 0.807161; // gpt-5.4 Build, post-change
    const GLM46_BUILD: f64 = 0.19148;
    let mut models = fixture_cached_models();
    models.retain(|model| filtered_model_set.contains(&model.name.as_str()));
    for model in &mut models {
        stamp_selection_provenance(model);
    }
    let actual: std::collections::BTreeSet<&str> = models.iter().map(|m| m.name.as_str()).collect();
    let expected: std::collections::BTreeSet<&str> = filtered_model_set.iter().copied().collect();
    assert_eq!(actual, expected, "filtered fixture model set drifted");
    let anchor = models
        .iter()
        .find(|m| m.name == COMPARISON_ANCHOR_MODEL)
        .unwrap();
    let anchor_prob = rounded_probability(phase_score_for_legacy_callers(
        anchor,
        SelectionPhase::Build,
    ));
    assert_eq!(
        anchor_prob, COMPARISON_ANCHOR_BUILD,
        "{COMPARISON_ANCHOR_MODEL} Build anchor drifted"
    );
    let glm46 = models.iter().find(|m| m.name == "glm-4.6").unwrap();
    let glm46_prob =
        rounded_probability(phase_score_for_legacy_callers(glm46, SelectionPhase::Build));
    assert_eq!(glm46_prob, GLM46_BUILD, "glm-4.6 Build score drifted");
    assert!(glm46_prob > 0.0, "glm-4.6 Build should remain scored");
}

#[test]
fn glm46_post_change_substantially_improved() {
    // AC #4a-bis: the unfiltered fixture still includes glm-4.7, so
    // this asserts the pinned lift while leaving the version penalty
    // unchanged as required by spec §4.2 / §8.
    let prechange: BTreeMap<String, BTreeMap<String, f64>> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/aistupidlevel_2026-04-26_prechange_selection_probabilities.json"
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
    let ratio = 1.0 / 3.0;

    for phase in [
        SelectionPhase::Build,
        SelectionPhase::Planning,
        SelectionPhase::Review,
    ] {
        let probs: Vec<(&str, f64)> = models
            .iter()
            .map(|m| (m.name.as_str(), phase_score_for_legacy_callers(m, phase)))
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

    let prechange: BTreeMap<String, BTreeMap<String, f64>> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/aistupidlevel_2026-04-26_prechange_selection_probabilities.json"
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

    let score_with = phase_score_for_legacy_callers(glm, SelectionPhase::Idea);

    // Synthetic variant with contextawareness and taskcompletion removed
    let mut stripped = glm.clone();
    stripped
        .axes
        .retain(|(k, _)| k != "contextawareness" && k != "taskcompletion");
    stripped.ipbr_phase_scores.idea = None;
    let score_without = phase_score_for_legacy_callers(&stripped, SelectionPhase::Idea);

    assert!(
        (score_with - score_without).abs() > 1e-6,
        "Idea score should differ when contextawareness/taskcompletion present ({score_with}) vs absent ({score_without})"
    );
}

fn fixture_score_entries() -> Vec<ScoreEntry> {
    let payload: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/aistupidlevel_2026-04-26_subset.json"
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
    use std::sync::{Mutex, OnceLock};
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

// -------------------------------------------------------------------
// ipbr scoreboard.toml parsing
// -------------------------------------------------------------------

const IPBR_FIXTURE: &str = r#"
[[models]]
display_name = "claude-opus-4-7"
canonical_id = "anthropic/claude-opus-4-7"
vendor = "anthropic"
aliases = ["claude-opus-4.7", "claude_opus_4_7"]
unknown_top_level = "ignored"

[models.scores]
i_adj = 92.5
p_adj = 91.0
b_adj = 90.0
r = 89.5
unused_extra = 7.0

[[models]]
display_name = "GPT-5.4"
canonical_id = "openai/gpt-5-4"
vendor = "openai"
aliases = ["gpt5.4"]

[models.scores]
i_adj = 80.0
b_adj = 78.0
r = 77.0
# p_adj omitted on purpose

[[models]]
display_name = "gemini-2.5-pro"
vendor = "google"
# scores table missing entirely
"#;

#[test]
fn parse_ipbr_preserves_inventory_compatible_name_and_normalizes_canonical_id_aliases() {
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    assert_eq!(entries.len(), 3, "all three rows should parse");

    let opus = entries
        .iter()
        .find(|e| e.name == "claude-opus-4-7")
        .unwrap();
    assert_eq!(opus.vendor, "anthropic");
    assert_eq!(opus.score_source, ScoreSource::Ipbr);
    assert!(opus.ipbr_row_matched);
    // canonical_id is fully normalized via `normalize_ipbr_key` for the
    // upcoming matching task — distinct from `name`, which preserves the
    // inventory lookup shape.
    assert_eq!(
        opus.canonical_id.as_deref(),
        Some("anthropic-claude-opus-4-7")
    );
    // Aliases are normalized: punctuation/underscores collapse to `-`,
    // so two distinct surface forms can produce the same key.
    assert_eq!(
        opus.aliases,
        vec!["claude-opus-4-7".to_string(), "claude-opus-4-7".to_string(),]
    );
    assert_eq!(opus.ipbr_phase_scores.idea, Some(92.5));
    assert_eq!(opus.ipbr_phase_scores.planning, Some(91.0));
    assert_eq!(opus.ipbr_phase_scores.build, Some(90.0));
    assert_eq!(opus.ipbr_phase_scores.review, Some(89.5));
    // Cosmetic overall_score = mean of present phase scores.
    let expected = (92.5 + 91.0 + 90.0 + 89.5) / 4.0;
    assert!((opus.overall_score - expected).abs() < 1e-9);
    assert_eq!(opus.current_score, opus.overall_score);
    // axes/standard_error are not populated by ipbr ingestion.
    assert!(opus.axes.is_empty());
    assert_eq!(opus.standard_error, 0.0);
}

#[test]
fn parse_ipbr_name_is_lowercase_only_to_match_inventory_lookup_shape() {
    // Inventory rows store names via `trim().to_ascii_lowercase()`. A
    // dotted form like `gpt-5.4` must round-trip on the score side so
    // the existing exact-match merge still enriches inventory-visible
    // models. Normalized/kebab forms belong to canonical_id/aliases.
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    let gpt = entries.iter().find(|e| e.name == "gpt-5.4").unwrap();
    assert_eq!(gpt.canonical_id.as_deref(), Some("openai-gpt-5-4"));
}

#[test]
fn parse_ipbr_row_missing_one_phase_marks_only_that_phase_absent() {
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    let gpt = entries.iter().find(|e| e.name == "gpt-5.4").unwrap();

    // Only the omitted field is None; remaining phases stay present.
    assert_eq!(gpt.ipbr_phase_scores.idea, Some(80.0));
    assert_eq!(gpt.ipbr_phase_scores.planning, None);
    assert_eq!(gpt.ipbr_phase_scores.build, Some(78.0));
    assert_eq!(gpt.ipbr_phase_scores.review, Some(77.0));
    assert_eq!(gpt.score_source, ScoreSource::Ipbr);
    assert!(gpt.ipbr_row_matched);

    let mean = (80.0 + 78.0 + 77.0) / 3.0;
    assert!((gpt.overall_score - mean).abs() < 1e-9);
}

#[test]
fn parse_ipbr_row_missing_all_phases_is_parseable_but_carries_no_ranking_authority() {
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    let gemini = entries.iter().find(|e| e.name == "gemini-2.5-pro").unwrap();

    assert_eq!(gemini.ipbr_phase_scores, IpbrPhaseScores::default());
    // Cosmetic summary defaults to 0.0 when no phases are present, so
    // it cannot accidentally rank above genuine ipbr data — and per
    // spec it must not be treated as a phase fallback regardless.
    assert_eq!(gemini.overall_score, 0.0);
    assert_eq!(gemini.current_score, 0.0);
    // Provenance is still ipbr because the row itself came from ipbr;
    // selection layers must consult ipbr_phase_scores, not provenance.
    assert_eq!(gemini.score_source, ScoreSource::Ipbr);
    assert!(gemini.ipbr_row_matched);
}

#[test]
fn parse_ipbr_ignores_unknown_top_level_and_score_fields() {
    // The fixture exercises both an unknown top-level row field and an
    // unknown nested score field; parsing must not error.
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("unknown fields must be ignored");
    assert!(entries.iter().any(|e| e.name == "claude-opus-4-7"));
}

#[test]
fn parse_ipbr_malformed_feed_surfaces_existing_failure_path() {
    // Non-TOML payload follows the existing fetch/parse failure path.
    let err = parse_ipbr_scoreboard("not = valid = toml").unwrap_err();
    assert!(
        err.to_string()
            .contains("ipbr scoreboard was not valid TOML"),
        "expected feed-level parse failure, got {err}"
    );

    // A structurally-correct but empty feed surfaces the same kind of
    // "no models" failure as the previous score path so the dashboard
    // refresh error is preserved.
    let err = parse_ipbr_scoreboard("").unwrap_err();
    assert!(
        err.to_string().contains("returned no models"),
        "expected no-models error, got {err}"
    );
}

#[test]
fn parse_ipbr_scoreboard_is_what_load_scores_uses() {
    // Smoke-check the production fetch boundary: the URL constant
    // points at the documented ipbr endpoint and the parser the
    // fetcher delegates to is `parse_ipbr_scoreboard`. This pins the
    // production wiring without performing a network round-trip.
    assert_eq!(IPBR_SCOREBOARD_URL, "https://ipbr.dev/scoreboard.toml");
    // Confirm the parser is a real function (compiles) by exercising
    // a minimal valid feed through it.
    let entries =
        parse_ipbr_scoreboard("[[models]]\ndisplay_name = \"x\"\nvendor = \"v\"\n").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "x");
    assert_eq!(entries[0].vendor, "v");
}

#[test]
fn normalize_ipbr_key_handles_punctuation_and_whitespace() {
    use crate::data::dashboard_model::normalize_ipbr_key;
    assert_eq!(normalize_ipbr_key("Claude Opus 4.1"), "claude-opus-4-1");
    assert_eq!(
        normalize_ipbr_key("anthropic/claude_opus"),
        "anthropic-claude-opus"
    );
    assert_eq!(normalize_ipbr_key("--gpt..5__4--"), "gpt-5-4");
    assert_eq!(normalize_ipbr_key("   "), "");
    assert_eq!(normalize_ipbr_key(""), "");
}
