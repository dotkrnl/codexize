use super::*;

const VERBOSE_FIXTURE: &str = r#"opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "family": "big-pickle",
  "api": {
    "id": "big-pickle",
    "url": "https://opencode.ai/zen/v1",
    "npm": "@ai-sdk/anthropic"
  },
  "status": "active",
  "limit": { "context": 200000, "output": 128000 }
}
opencode/gpt-5-nano
{
  "id": "gpt-5-nano",
  "providerID": "opencode",
  "name": "GPT-5 Nano",
  "family": "gpt-nano",
  "api": {
    "id": "gpt-5-nano",
    "url": "https://opencode.ai/zen/v1",
    "npm": "@ai-sdk/openai"
  },
  "status": "active"
}
opencode/kimi-something
{
  "id": "kimi-something",
  "providerID": "opencode",
  "api": {
    "npm": "@ai-sdk/moonshotai"
  }
}
"#;

const STATS_FIXTURE: &str = r#"┌────────────────────────────────────────────────────────┐
│                       OVERVIEW                         │
├────────────────────────────────────────────────────────┤
│Sessions                                              2 │
│Messages                                            122 │
│Days                                                 30 │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│                    COST & TOKENS                       │
├────────────────────────────────────────────────────────┤
│Total Cost                                        $1.80 │
│Avg Cost/Day                                      $0.06 │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│                      MODEL USAGE                       │
├────────────────────────────────────────────────────────┤
│ opencode-go/glm-5.1                                    │
│  Messages                                           62 │
│  Cost                                          $1.7649 │
├────────────────────────────────────────────────────────┤
│ opencode-go/deepseek-v4-flash                          │
│  Messages                                           49 │
│  Cost                                          $0.0313 │
├────────────────────────────────────────────────────────┤
│ openrouter/some-other-model                            │
│  Messages                                           12 │
│  Cost                                          $9.9999 │
└────────────────────────────────────────────────────────┘
"#;

#[test]
fn parse_verbose_extracts_each_block() {
    let models = parse_verbose_models(VERBOSE_FIXTURE);
    assert_eq!(models.len(), 3, "expected 3 model blocks: {models:?}");
    assert_eq!(models[0].id, "big-pickle");
    assert_eq!(models[0].provider_id, "opencode");
    assert_eq!(models[0].display_name.as_deref(), Some("Big Pickle"));
    assert_eq!(models[0].underlying_vendor, Some(VendorKind::Claude));
    assert_eq!(models[1].id, "gpt-5-nano");
    assert_eq!(models[1].underlying_vendor, Some(VendorKind::Codex));
    assert_eq!(models[2].id, "kimi-something");
    assert_eq!(models[2].underlying_vendor, Some(VendorKind::Kimi));
}

#[test]
fn parse_verbose_handles_braces_inside_strings() {
    let fixture = r#"opencode/quoted
{
  "id": "quoted",
  "providerID": "opencode",
  "note": "this string has a } inside",
  "api": { "npm": "@ai-sdk/openai" }
}
"#;
    let models = parse_verbose_models(fixture);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "quoted");
}

#[test]
fn parse_verbose_returns_empty_for_garbage() {
    assert!(parse_verbose_models("").is_empty());
    assert!(parse_verbose_models("not a JSON document").is_empty());
}

#[test]
fn enumerate_falls_back_to_hardcoded_when_cli_text_missing() {
    let models = enumerate_with_cli_text(None);
    assert!(!models.is_empty(), "fallback list must not be empty");
    assert!(models.iter().all(|m| m.provider_id == "opencode"));
    assert!(models.iter().any(|m| m.id == "gpt-5-nano"));
}

#[test]
fn enumerate_falls_back_when_cli_text_parses_to_nothing() {
    let models = enumerate_with_cli_text(Some("nothing parseable here"));
    assert!(!models.is_empty());
    assert!(models.iter().any(|m| m.id == "gpt-5-nano"));
}

#[test]
fn enumerate_prefers_cli_text_over_fallback() {
    let models = enumerate_with_cli_text(Some(VERBOSE_FIXTURE));
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["big-pickle", "gpt-5-nano", "kimi-something"]);
}

#[test]
fn extract_go_tier_spend_sums_only_opencode_go_rows() {
    let spent = extract_go_tier_spend(STATS_FIXTURE).unwrap();
    // 1.7649 + 0.0313 = 1.7962
    assert!(
        (spent - 1.7962).abs() < 1e-9,
        "expected go-tier spend 1.7962, got {spent}"
    );
}

#[test]
fn extract_go_tier_spend_is_zero_when_no_go_rows() {
    let fixture = r#"┌─┐
│       MODEL USAGE       │
├─┤
│ openrouter/some-model    │
│  Cost                  $4.20 │
└─┘"#;
    let spent = extract_go_tier_spend(fixture).unwrap();
    assert_eq!(spent, 0.0);
}

#[test]
fn extract_go_tier_spend_errors_when_table_missing() {
    let err = extract_go_tier_spend("no model usage rendered here").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("MODEL USAGE"),
        "error should mention missing section: {msg}"
    );
}

#[test]
fn remaining_percent_clamps_and_rounds() {
    assert_eq!(remaining_percent_from_spend(0.0), 100);
    assert_eq!(remaining_percent_from_spend(60.0), 0);
    // Anything past the cap stays clamped at 0% rather than going negative.
    assert_eq!(remaining_percent_from_spend(120.0), 0);
    assert_eq!(remaining_percent_from_spend(-5.0), 100);
    // 30 spent → 30/60 = 50% remaining.
    assert_eq!(remaining_percent_from_spend(30.0), 50);
    // 1.7962 spent → (60 - 1.7962) / 60 = 0.97006... → 97%.
    assert_eq!(remaining_percent_from_spend(1.7962), 97);
    // Just over the half-percent boundary rounds up to 1%.
    // 59.6 spent → 0.4 remaining → 0.666...% → round → 1.
    assert_eq!(remaining_percent_from_spend(59.6), 1);
    // Just under it rounds down to 0%.
    // 59.8 spent → 0.2 remaining → 0.333...% → round → 0.
    assert_eq!(remaining_percent_from_spend(59.8), 0);
}

#[test]
fn remaining_percent_handles_nonfinite() {
    assert_eq!(remaining_percent_from_spend(f64::NAN), 0);
    assert_eq!(remaining_percent_from_spend(f64::INFINITY), 0);
}

#[test]
fn quota_models_from_stats_emits_shared_key() {
    let models = quota_models_from_stats(STATS_FIXTURE).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, SHARED_QUOTA_KEY);
    assert_eq!(models[0].quota_percent, Some(97));
    assert!(models[0].quota_resets_at.is_none());
}

#[test]
fn quota_models_from_stats_propagates_missing_table_error() {
    let err = quota_models_from_stats("garbage with no MODEL section").unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("MODEL USAGE"), "error should propagate: {msg}");
}
