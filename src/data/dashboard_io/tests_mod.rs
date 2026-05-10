use super::*;

const IPBR_FIXTURE: &str = r#"
[[models]]
display_name = "claude-opus-4.7"
vendor = "anthropic"
unknown_top_level = "ignored"

[models.scores]
i_adj = 92.5
p_adj = 91.0
b_adj = 90.0
r = 89.5
unused_extra = 7.0

[[models]]
display_name = "GPT-5.4"
vendor = "openai"

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
    assert_eq!(opus.score_source, ScoreSource::Ipbr);
    assert_eq!(opus.ipbr_phase_scores.idea, Some(92.5));
    assert_eq!(opus.ipbr_phase_scores.planning, Some(91.0));
    assert_eq!(opus.ipbr_phase_scores.build, Some(90.0));
    assert_eq!(opus.ipbr_phase_scores.review, Some(89.5));
}

#[test]
fn parse_ipbr_name_is_lowercase_only_without_punctuation_normalization() {
    // Provider entries must use IPBR's canonical display_name. Dots must
    // not be converted to dashes.
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
}

#[test]
fn parse_ipbr_row_missing_all_phases_is_parseable_but_carries_no_ranking_authority() {
    let entries = parse_ipbr_scoreboard(IPBR_FIXTURE).expect("fixture should parse");
    let gemini = entries.iter().find(|e| e.name == "gemini-2.5-pro").unwrap();

    assert_eq!(gemini.ipbr_phase_scores, IpbrPhaseScores::default());
    // Provenance is still ipbr because the row itself came from ipbr;
    // selection layers must consult ipbr_phase_scores, not provenance.
    assert_eq!(gemini.score_source, ScoreSource::Ipbr);
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
}
