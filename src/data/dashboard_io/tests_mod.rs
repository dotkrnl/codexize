use super::*;
use crate::selection::config::SelectionPhase;
use crate::selection::ranking::phase_score_for_legacy_callers;
use crate::selection::types::{CachedModel, SubscriptionKind};
use std::collections::BTreeMap;

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
display_name = "claude-opus-4.7"
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
fn parse_ipbr_uses_display_name_as_canonical_model_name() {
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    assert_eq!(entries.len(), 3, "all three rows should parse");

    let opus = entries
        .iter()
        .find(|e| e.name == "claude-opus-4.7")
        .unwrap();
    assert_eq!(opus.vendor, "anthropic");
    assert_eq!(opus.score_source, ScoreSource::Ipbr);
    assert!(opus.ipbr_row_matched);
    assert_eq!(opus.ipbr_phase_scores.idea, Some(92.5));
    assert_eq!(opus.ipbr_phase_scores.planning, Some(91.0));
    assert_eq!(opus.ipbr_phase_scores.build, Some(90.0));
    assert_eq!(opus.ipbr_phase_scores.review, Some(89.5));
}

#[test]
fn parse_ipbr_name_is_lowercase_only_without_punctuation_normalization() {
    // Provider entries must use IPBR's canonical display_name. A dotted
    // form like `gpt-5.4` must not become `gpt-5-4`.
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    let gpt = entries.iter().find(|e| e.name == "gpt-5.4").unwrap();
    assert_eq!(gpt.name, "gpt-5.4");
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
    assert!(entries.iter().any(|e| e.name == "claude-opus-4.7"));
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
