use super::*;

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
fn extract_go_tier_spend_errors_when_go_row_has_opaque_cost() {
    let fixture = r#"┌─┐
│       MODEL USAGE       │
├─┤
│ opencode-go/glm-5.1     │
│  Messages            62 │
│  Cost             tokens │
└─┘"#;
    let err = extract_go_tier_spend(fixture).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("opencode-go/glm-5.1") && msg.contains("dollar"),
        "error should identify unsupported Go-tier quota shape: {msg}"
    );
}

#[test]
fn quota_models_from_stats_propagates_opaque_go_cost_error() {
    let fixture = r#"┌─┐
│       MODEL USAGE       │
├─┤
│ opencode-go/glm-5.1     │
│  Messages            62 │
│  Cost             quota │
└─┘"#;
    let err = quota_models_from_stats(fixture).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("opencode-go/glm-5.1") && msg.contains("dollar"),
        "quota construction should surface unsupported Go-tier shape: {msg}"
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
