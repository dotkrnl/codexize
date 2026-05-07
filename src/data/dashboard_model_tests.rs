use super::*;

fn score(name: &str, value: f64, order: usize) -> ScoreEntry {
    ScoreEntry {
        name: name.to_string(),
        vendor: "vendor".to_string(),
        overall_score: value,
        current_score: value,
        standard_error: 0.0,
        axes: vec![("correctness".to_string(), value)],
        axis_provenance: BTreeMap::new(),
        display_order: order,
        canonical_id: None,
        aliases: Vec::new(),
        ipbr_phase_scores: IpbrPhaseScores::default(),
        score_source: ScoreSource::None,
        ipbr_row_matched: false,
    }
}

fn ipbr_score(
    name: &str,
    canonical_id: Option<&str>,
    aliases: &[&str],
    value: f64,
    order: usize,
) -> ScoreEntry {
    ScoreEntry {
        canonical_id: canonical_id.map(normalize_ipbr_key),
        aliases: aliases
            .iter()
            .map(|alias| normalize_ipbr_key(alias))
            .collect(),
        ipbr_phase_scores: IpbrPhaseScores {
            idea: Some(value),
            planning: Some(value + 1.0),
            build: Some(value + 2.0),
            review: Some(value + 3.0),
        },
        score_source: ScoreSource::Ipbr,
        ipbr_row_matched: true,
        ..score(name, value, order)
    }
}

fn inventory(name: &str, _order: usize) -> InventoryEntry {
    InventoryEntry {
        name: name.to_string(),
        vendor: String::new(),
        route_underlying_vendor: None,
        route_provider: None,
    }
}

fn vendor_inventory(name: &str, vendor: &str, _order: usize) -> InventoryEntry {
    InventoryEntry {
        name: name.to_string(),
        vendor: vendor.to_string(),
        route_underlying_vendor: None,
        route_provider: None,
    }
}

#[test]
fn merge_drops_inventory_without_ipbr_match_and_admits_orphan_scores() {
    // ipbr is the sole authority for the supported universe: an inventory
    // row without an ipbr match disappears, while an ipbr score with no
    // inventory still surfaces directly under its scoreboard vendor.
    let models = merge(
        vec![inventory("gpt-5.5", 0), inventory("claude-sonnet-4.5", 1)],
        vec![ipbr_score("gpt-5.4", None, &[], 0.8, 0)],
    );

    assert!(
        !models.iter().any(|m| m.name == "gpt-5.5"),
        "inventory rows without an ipbr match should not be kept"
    );
    assert!(
        !models.iter().any(|m| m.name == "claude-sonnet-4.5"),
        "inventory rows without an ipbr match should not be kept"
    );
    assert!(
        models.iter().any(|m| m.name == "gpt-5.4"),
        "ipbr scores with no matching inventory still surface"
    );
}

#[test]
fn merge_matches_inventory_by_normalized_ipbr_aliases() {
    let models = merge(
        vec![inventory("claude-opus-4.1", 0)],
        vec![ipbr_score(
            "Claude Opus 4",
            None,
            &["claude_opus_4_1"],
            91.0,
            3,
        )],
    );

    let model = models
        .iter()
        .find(|model| model.name == "claude-opus-4.1")
        .expect("inventory model should remain visible");

    assert_eq!(model.score_source, ScoreSource::Ipbr);
    assert_eq!(model.ipbr_phase_scores.build, Some(93.0));
    assert_eq!(model.fallback_from, None);
}

#[test]
fn merge_does_not_readd_ipbr_row_consumed_by_normalized_inventory_match() {
    let models = merge(
        vec![inventory("claude-opus-4.1", 0)],
        vec![ipbr_score("Claude Opus 4.1", None, &[], 91.0, 3)],
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-opus-4.1");
    assert_eq!(models[0].score_source, ScoreSource::Ipbr);
    assert_eq!(models[0].ipbr_phase_scores.build, Some(93.0));
}

#[test]
fn merge_matches_inventory_by_normalized_provider_path_aliases() {
    let models = merge(
        vec![inventory("anthropic/claude-opus-4", 0)],
        vec![ipbr_score(
            "Claude Opus 4",
            Some("anthropic/claude-opus-4"),
            &[],
            88.0,
            2,
        )],
    );

    let model = models
        .iter()
        .find(|model| model.name == "anthropic/claude-opus-4")
        .expect("provider-path inventory model should remain visible");

    assert_eq!(model.score_source, ScoreSource::Ipbr);
    assert_eq!(model.ipbr_phase_scores.review, Some(91.0));
    assert_eq!(model.fallback_from, None);
}

#[test]
fn merge_excludes_collided_normalized_ipbr_keys_and_warns() {
    let result = merge_with_warnings(
        vec![inventory("claude-opus-4.1", 0)],
        vec![
            ipbr_score("Claude Opus 4.1", None, &[], 90.0, 1),
            ipbr_score("Other Opus", None, &["claude_opus_4_1"], 70.0, 2),
        ],
    );

    // Both colliding scores are excluded from the lookup, so the inventory
    // row has nothing to match and is dropped. The collision still surfaces
    // as a warning so operators see the upstream feed problem.
    assert!(
        !result.models.iter().any(|m| m.name == "claude-opus-4.1"),
        "inventory rows whose only matches collided should be dropped"
    );
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("claude-opus-4-1"));
}

#[test]
fn merge_drops_opencode_inventory_without_ipbr_match() {
    let models = merge(
        vec![
            vendor_inventory("gpt-5-nano", "opencode", 0),
            vendor_inventory("opencode-only-model", "opencode", 1),
        ],
        vec![ipbr_score("gpt-5-nano", None, &[], 86.0, 1)],
    );

    assert!(
        models.iter().any(|model| model.name == "gpt-5-nano"),
        "ipbr-matched opencode inventory should remain visible"
    );
    assert!(
        !models
            .iter()
            .any(|model| model.name == "opencode-only-model"),
        "opencode inventory with no ipbr row is outside the supported universe"
    );
}

#[test]
fn merge_no_longer_applies_in_merge_sibling_synthesis() {
    // The merge layer used to fall back to a same-stem sibling when an
    // inventory row had no ipbr match. ipbr is now the sole authority for
    // the supported universe, so the row is dropped instead. The public
    // `synthesize_sibling` helper (used by `assemble_universe` for live-quota
    // orphans) is still exercised below.
    let models = merge(
        vec![inventory("gpt-5.5", 0)],
        vec![ipbr_score("gpt-5.4", None, &[], 86.0, 1)],
    );
    assert!(!models.iter().any(|m| m.name == "gpt-5.5"));
}

#[test]
fn scores_only_converts_scores_without_fallback() {
    let models = scores_only(vec![score("gpt-5.4", 0.8, 2)]);

    assert_eq!(models[0].name, "gpt-5.4");
    assert_eq!(models[0].fallback_from, None);
}

#[test]
fn synthesize_sibling_uses_same_stem_score() {
    let existing = scores_only(vec![score("gpt-5.4", 0.8, 0)]);
    let synthesized = synthesize_sibling("gpt-5.5", "", &existing).unwrap();

    assert_eq!(synthesized.fallback_from.as_deref(), Some("gpt-5.4"));
    assert_eq!(synthesized.overall_score, 0.8);
}

#[test]
fn merged_axes_returns_axes_provenance_and_events() {
    let val = serde_json::json!([
        {
            "suite": "deep",
            "axes": {
                "contextWindow": 1.0,
                "correctness": "bad"
            }
        },
        {
            "suite": "tooling",
            "axes": { "efficiency": "0.42" }
        }
    ]);
    let (axes, provenance, events) = merged_axes(&val).unwrap();

    assert_eq!(axes, vec![("efficiency".to_string(), 0.42)]);
    assert_eq!(
        provenance.get("contextwindow").map(String::as_str),
        Some("dropped:contextwindow")
    );
    assert!(events.iter().any(|event| matches!(
        event,
        IngestEvent::AxisDropped { reason } if reason == "contextwindow"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        IngestEvent::AxisParseFail { suite, axis }
            if suite == "deep" && axis == "correctness"
    )));
}

#[test]
fn value_to_f64_accepts_numbers_strings_and_bools() {
    assert_eq!(value_to_f64(Some(&serde_json::json!(0.25))), Some(0.25));
    assert_eq!(value_to_f64(Some(&serde_json::json!("0.5"))), Some(0.5));
    assert_eq!(value_to_f64(Some(&serde_json::json!(true))), Some(1.0));
}

#[test]
fn value_to_string_preserves_strings_and_serializes_other_values() {
    assert_eq!(value_to_string(&serde_json::json!("abc")), "abc");
    assert_eq!(value_to_string(&serde_json::json!(7)), "7");
}

fn render_dashboard_models(models: &[DashboardModel]) -> String {
    // Hand-rolled rendering keeps the snapshot stable across Rust
    // versions that may format Debug-derived floats differently. We
    // also pin the order by name so HashMap-derived merges don't
    // make the snapshot ordering-sensitive.
    let mut sorted: Vec<&DashboardModel> = models.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for model in sorted {
        out.push_str(&format!("- name: {}\n", model.name));
        out.push_str(&format!("  vendor: {}\n", model.vendor));
        out.push_str(&format!("  overall_score: {:.4}\n", model.overall_score));
        out.push_str(&format!("  current_score: {:.4}\n", model.current_score));
        out.push_str(&format!("  standard_error: {:.4}\n", model.standard_error));
        out.push_str(&format!("  display_order: {}\n", model.display_order));
        out.push_str(&format!("  score_source: {:?}\n", model.score_source));
        out.push_str(&format!("  ipbr_row_matched: {}\n", model.ipbr_row_matched));
        out.push_str(&format!(
            "  fallback_from: {}\n",
            model.fallback_from.as_deref().unwrap_or("-")
        ));
        out.push_str("  axes:\n");
        for (axis, value) in &model.axes {
            out.push_str(&format!("    - {}: {:.4}\n", axis, value));
        }
        out.push_str(&format!(
            "  ipbr_phase_scores: idea={:?} planning={:?} build={:?} review={:?}\n",
            model.ipbr_phase_scores.idea,
            model.ipbr_phase_scores.planning,
            model.ipbr_phase_scores.build,
            model.ipbr_phase_scores.review,
        ));
    }
    out
}

#[test]
fn dashboard_model_after_representative_merge_snapshot() {
    // Mirrors a typical refresh: a couple of inventory rows joined against
    // a small set of scores. Inventory rows without an ipbr match are
    // dropped; ipbr-sourced rows survive with phase scores; legacy
    // non-ipbr scores still surface (only test fixtures produce these).
    let models = merge(
        vec![
            inventory("anthropic/claude-opus-4", 0),
            inventory("gpt-5.5", 1),
            inventory("claude-sonnet-4.5", 2),
        ],
        vec![
            ipbr_score(
                "Claude Opus 4",
                Some("anthropic/claude-opus-4"),
                &[],
                88.0,
                2,
            ),
            score("gpt-5.4", 0.8, 5),
        ],
    );
    insta::assert_snapshot!(
        "dashboard_model_after_representative_merge",
        render_dashboard_models(&models)
    );
}
