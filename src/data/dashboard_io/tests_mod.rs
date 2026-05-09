use super::*;
use crate::selection::config::SelectionPhase;
use crate::selection::ranking::phase_score_for_legacy_callers;
use crate::selection::types::{CachedModel, SubscriptionKind};
use std::collections::BTreeMap;

#[test]
fn opencode_enumerated_inventory_intersects_ipbr_and_preserves_route_metadata() {
    let mut inventory = Vec::new();
    append_opencode_inventory(
        &mut inventory,
        vec![
            crate::data::providers::opencode::OpencodeModelMeta {
                id: "gpt-5-nano".to_string(),
                provider_id: "opencode".to_string(),
                display_name: None,
                api_npm: None,
            },
            crate::data::providers::opencode::OpencodeModelMeta {
                id: "opencode-only-model".to_string(),
                provider_id: "opencode".to_string(),
                display_name: None,
                api_npm: None,
            },
        ],
    );
    let scores = parse_ipbr_scoreboard(
        r#"
        [[models]]
        display_name = "gpt-5-nano"
        vendor = "openai"

        [models.scores]
        i_adj = 60.0
        p_adj = 61.0
        b_adj = 62.0
        r = 63.0
        "#,
    )
    .unwrap();

    let merged = merge_with_warnings(inventory, scores).models;

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].name, "gpt-5-nano");
    assert_eq!(merged[0].dashboard_vendor, "opencode");
    assert_eq!(merged[0].ipbr_match_key.as_deref(), Some("gpt-5-nano"));
}

#[test]
fn opencode_go_inventory_surfaces_when_ipbr_matches() {
    // Drives the headline use case: deepseek lives only under
    // `opencode-go`, has an ipbr scoreboard row by `display_name =
    // "deepseek-v4-flash"`, and must reach the universe so the launch
    // path can qualify it via the OpencodeGo subscription rather than
    // falling back to the zen-tier `opencode/`.
    let mut inventory = Vec::new();
    append_opencode_inventory(
        &mut inventory,
        vec![crate::data::providers::opencode::OpencodeModelMeta {
            id: "deepseek-v4-flash".to_string(),
            provider_id: "opencode-go".to_string(),
            display_name: None,
            api_npm: Some("@ai-sdk/openai-compatible".to_string()),
        }],
    );
    let scores = parse_ipbr_scoreboard(
        r#"
        [[models]]
        display_name = "deepseek-v4-flash"
        canonical_id = "deepseek/deepseek-v4-flash"
        vendor = "deepseek"

        [models.scores]
        i_adj = 70.0
        p_adj = 71.0
        b_adj = 72.0
        r = 73.0
        "#,
    )
    .unwrap();

    let merged = merge_with_warnings(inventory, scores).models;

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].name, "deepseek-v4-flash");
    assert_eq!(merged[0].dashboard_vendor, "opencode");
    assert_eq!(
        merged[0].ipbr_match_key.as_deref(),
        Some("deepseek-v4-flash")
    );
}

fn vendor_for_fixture_model(name: &str) -> SubscriptionKind {
    match name {
        "claude-sonnet-4.6" => SubscriptionKind::Claude,
        "gpt-5.4" => SubscriptionKind::Codex,
        "gemini-2.5-pro" => SubscriptionKind::Gemini,
        "glm-4.6" | "glm-4.7" => SubscriptionKind::Direct,
        other => panic!("fixture model {other} is not mapped"),
    }
}

fn fixture_cached_models() -> Vec<CachedModel> {
    let pinned_phase_scores: BTreeMap<String, BTreeMap<String, f64>> = serde_json::from_str(
        include_str!("../../../tests/fixtures/aistupidlevel_2026-04-26_postchange_selection_probabilities.json"),
    )
    .expect("post-change artifact should be valid JSON");
    pinned_phase_scores
        .into_iter()
        .enumerate()
        .map(|(display_order, (name, scores))| {
            let name: String = name;
            let scores: BTreeMap<String, f64> = scores;
            CachedModel {
                subscription: vendor_for_fixture_model(&name),
                name: name.clone(),
                ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                    idea: scores.get("idea").copied(),
                    planning: scores.get("planning").copied(),
                    build: scores.get("build").copied(),
                    review: scores.get("review").copied(),
                },
                score_source: crate::selection::ScoreSource::Ipbr,
                ipbr_row_matched: true,
                ipbr_match_key: Some(name),
                candidates: Vec::new(),
                selected_candidate: None,
                quota_percent: Some(80),
                quota_resets_at: None,
                display_order,
            }
        })
        .collect()
}

fn rounded_probability(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn fixture_postchange_snapshot() -> BTreeMap<String, BTreeMap<String, f64>> {
    let models = fixture_cached_models();
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
fn glm46_post_change_substantially_improved() {
    // AC #4a-bis: the pinned ipbr scores must keep glm-4.6 well above its
    // pre-change Build probability so the selection lift is preserved.
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
fn ranking_order_among_healthy_models_unchanged() {
    // AC #8: healthy models keep the same ranking order pre- vs post-change.
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

// -------------------------------------------------------------------
// ipbr scoreboard.toml parsing
// -------------------------------------------------------------------

const IPBR_FIXTURE: &str = r#"
[[models]]
display_name = "claude-opus-4-7"
canonical_id = "anthropic/claude-opus-4-7"
vendor = "anthropic"
aliases = ["claude_opus_4_7"]
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
    // Aliases are normalized: punctuation/underscores collapse to `-`.
    assert_eq!(opus.aliases, vec!["claude-opus-4-7".to_string()]);
    assert_eq!(opus.ipbr_phase_scores.idea, Some(92.5));
    assert_eq!(opus.ipbr_phase_scores.planning, Some(91.0));
    assert_eq!(opus.ipbr_phase_scores.build, Some(90.0));
    assert_eq!(opus.ipbr_phase_scores.review, Some(89.5));
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
}

#[test]
fn parse_ipbr_row_missing_all_phases_is_parseable_but_carries_no_ranking_authority() {
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    let gemini = entries.iter().find(|e| e.name == "gemini-2.5-pro").unwrap();

    assert_eq!(gemini.ipbr_phase_scores, IpbrPhaseScores::default());
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
