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
        VendorKind::Codex,
        VendorKind::Claude,
        VendorKind::Gemini,
        VendorKind::Kimi,
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
        effort_suffix(VendorKind::Codex, EffortLevel::Tough),
        ":xhigh"
    );
    assert_eq!(
        effort_suffix(VendorKind::Claude, EffortLevel::Tough),
        ":max"
    );
    assert_eq!(effort_suffix(VendorKind::Gemini, EffortLevel::Tough), "");
    assert_eq!(effort_suffix(VendorKind::Kimi, EffortLevel::Tough), "");
}

#[test]
fn effort_suffix_low_maps_provider_suffix() {
    assert_eq!(effort_suffix(VendorKind::Codex, EffortLevel::Low), ":low");
    assert_eq!(effort_suffix(VendorKind::Claude, EffortLevel::Low), ":low");
    assert_eq!(effort_suffix(VendorKind::Gemini, EffortLevel::Low), "");
    assert_eq!(effort_suffix(VendorKind::Kimi, EffortLevel::Low), "");
}

#[test]
fn run_label_with_model_appends_effort_suffix() {
    let name = run_label_with_model(
        "[Round 1 Coder]",
        "gpt-5.5",
        VendorKind::Codex,
        EffortLevel::Tough,
    );
    assert_eq!(name, "[Round 1 Coder] gpt-5.5:xhigh");
}
