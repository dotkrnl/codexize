use super::*;

#[test]
fn short_model_preserves_claude_prefix_behavior() {
    assert_eq!(short_model("claude-sonnet-4.6"), "sonnet-4.6");
    assert_eq!(short_model("gpt-5.2"), "gpt-5.2");
}

#[test]
fn short_model_uses_gemini_preview_display_label() {
    assert_eq!(short_model("gemini-3.1-pro-preview"), "3.1-pro");
}

#[test]
fn effort_suffix_normal_is_empty_for_all_vendors() {
    for vendor in [
        SubscriptionKind::Codex,
        SubscriptionKind::Claude,
        SubscriptionKind::Gemini,
        SubscriptionKind::Kimi,
        SubscriptionKind::OpencodeGo,
    ] {
        assert_eq!(
            effort_suffix(vendor, EffortLevel::Normal),
            "",
            "{vendor:?} Normal should produce empty suffix"
        );
    }
}

#[test]
fn effort_suffix_tough_maps_provider_suffix() {
    assert_eq!(
        effort_suffix(SubscriptionKind::Codex, EffortLevel::Tough),
        ":xhigh"
    );
    assert_eq!(
        effort_suffix(SubscriptionKind::Claude, EffortLevel::Tough),
        ":max"
    );
    assert_eq!(
        effort_suffix(SubscriptionKind::Gemini, EffortLevel::Tough),
        ""
    );
    assert_eq!(
        effort_suffix(SubscriptionKind::Kimi, EffortLevel::Tough),
        ""
    );
    assert_eq!(
        effort_suffix(SubscriptionKind::OpencodeGo, EffortLevel::Tough),
        ""
    );
}

#[test]
fn effort_suffix_low_maps_provider_suffix() {
    assert_eq!(
        effort_suffix(SubscriptionKind::Codex, EffortLevel::Low),
        ":low"
    );
    assert_eq!(
        effort_suffix(SubscriptionKind::Claude, EffortLevel::Low),
        ":low"
    );
    assert_eq!(
        effort_suffix(SubscriptionKind::Gemini, EffortLevel::Low),
        ""
    );
    assert_eq!(effort_suffix(SubscriptionKind::Kimi, EffortLevel::Low), "");
    assert_eq!(
        effort_suffix(SubscriptionKind::OpencodeGo, EffortLevel::Low),
        ""
    );
}

#[test]
fn opencode_effort_suffix_uses_underlying_vendor_when_known() {
    assert_eq!(
        effort_suffix_for_model(
            SubscriptionKind::OpencodeGo,
            Some(SubscriptionKind::Claude),
            "opencode/claude-opus-4.7",
            EffortLevel::Tough,
        ),
        ":max"
    );
    assert_eq!(
        effort_suffix_for_model(
            SubscriptionKind::OpencodeGo,
            Some(SubscriptionKind::Codex),
            "opencode/gpt-5.5",
            EffortLevel::Tough,
        ),
        ":xhigh"
    );
}

#[test]
fn opencode_effort_suffix_falls_back_to_model_name_heuristics() {
    assert_eq!(
        effort_suffix_for_model(
            SubscriptionKind::OpencodeGo,
            None,
            "opencode/claude-sonnet-4.6",
            EffortLevel::Low,
        ),
        ":low"
    );
    assert_eq!(
        effort_suffix_for_model(
            SubscriptionKind::OpencodeGo,
            None,
            "opencode/gemini-2.5-flash",
            EffortLevel::Tough,
        ),
        ""
    );
}

#[test]
fn run_label_with_model_appends_effort_suffix() {
    let name = run_label_with_model(
        "[Round 1 Coder]",
        "gpt-5.5",
        SubscriptionKind::Codex,
        EffortLevel::Tough,
    );
    assert_eq!(name, "[Round 1 Coder] gpt-5.5:xhigh");
}

#[test]
fn run_label_with_opencode_model_uses_name_fallback_suffix() {
    let name = run_label_with_model(
        "[Round 1 Coder]",
        "opencode/claude-opus-4.7",
        SubscriptionKind::OpencodeGo,
        EffortLevel::Tough,
    );
    assert_eq!(name, "[Round 1 Coder] opencode/claude-opus-4.7:max");
}
