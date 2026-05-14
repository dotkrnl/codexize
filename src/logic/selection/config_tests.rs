use super::*;

#[test]
fn vendor_bias_matches_opus_only_for_idea_and_planning() {
    let cfg = &SELECTION_CONFIG;
    assert_eq!(
        cfg.vendor_bias(
            SubscriptionKind::Claude,
            "claude-opus-4",
            SelectionStage::Idea
        ),
        1.5
    );
    assert_eq!(
        cfg.vendor_bias(
            SubscriptionKind::Claude,
            "claude-opus-4",
            SelectionStage::Planning
        ),
        1.5
    );
    // Non-opus Claude variants do not match the substring needle, so
    // the bias falls back to the neutral 1.0.
    assert_eq!(
        cfg.vendor_bias(
            SubscriptionKind::Claude,
            "claude-sonnet-4",
            SelectionStage::Idea
        ),
        1.0
    );
}

#[test]
fn vendor_bias_codex_review_uses_unrestricted_needle() {
    let cfg = &SELECTION_CONFIG;
    // The Codex Review entry has needle = None, so any model name
    // qualifies as long as the vendor + stage match.
    assert_eq!(
        cfg.vendor_bias(SubscriptionKind::Codex, "gpt-5.5", SelectionStage::Review),
        1.5
    );
    assert_eq!(
        cfg.vendor_bias(SubscriptionKind::Codex, "o1-mini", SelectionStage::Review),
        1.5
    );
    // Wrong stage: returns the neutral default.
    assert_eq!(
        cfg.vendor_bias(SubscriptionKind::Codex, "gpt-5.5", SelectionStage::Build),
        1.0
    );
}

#[test]
fn vendor_bias_unknown_vendor_stage_combo_is_one() {
    let cfg = &SELECTION_CONFIG;
    // Gemini and Kimi have no vendor_stage_biases entries.
    assert_eq!(
        cfg.vendor_bias(
            SubscriptionKind::Gemini,
            "gemini-2.5-pro",
            SelectionStage::Idea
        ),
        1.0
    );
    assert_eq!(
        cfg.vendor_bias(SubscriptionKind::Kimi, "kimi-k2", SelectionStage::Build),
        1.0
    );
}

#[test]
fn quota_weight_zero_or_negative_is_zero() {
    let cfg = &SELECTION_CONFIG;
    assert_eq!(cfg.quota_weight(0.0), 0.0);
    assert_eq!(cfg.quota_weight(-5.0), 0.0);
}

#[test]
fn quota_weight_at_or_above_soft_threshold_is_one() {
    let cfg = &SELECTION_CONFIG;
    let threshold = cfg.quota_soft_threshold;
    assert!((cfg.quota_weight(threshold) - 1.0).abs() < 1e-12);
    assert!((cfg.quota_weight(threshold * 4.0) - 1.0).abs() < 1e-12);
}

#[test]
fn quota_weight_is_concave_below_soft_threshold() {
    let cfg = &SELECTION_CONFIG;
    let threshold = cfg.quota_soft_threshold;
    let half = cfg.quota_weight(threshold / 2.0);
    // 1 - (1 - 0.5)^2 = 0.75
    assert!((half - 0.75).abs() < 1e-12, "quota_weight at half: {half}");
    let quarter = cfg.quota_weight(threshold / 4.0);
    // 1 - (1 - 0.25)^2 = 0.4375
    assert!(
        (quarter - 0.4375).abs() < 1e-12,
        "quota_weight at quarter: {quarter}"
    );
    // Strictly increasing on [0, threshold].
    assert!(quarter < half);
    assert!(half < cfg.quota_weight(threshold));
}
