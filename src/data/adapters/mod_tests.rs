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
fn launch_effort_suffix_normal_is_empty() {
    let mapping = EffortMapping::new("low", "medium", "xhigh");
    assert_eq!(
        launch_effort_suffix(EffortLevel::Normal, true, &mapping),
        ""
    );
}

#[test]
fn launch_effort_suffix_ineligible_is_empty() {
    let mapping = EffortMapping::new("low", "medium", "xhigh");
    // Even with Tough effort, an effort-incapable candidate gets no suffix.
    assert_eq!(
        launch_effort_suffix(EffortLevel::Tough, false, &mapping),
        ""
    );
    assert_eq!(launch_effort_suffix(EffortLevel::Low, false, &mapping), "");
}

#[test]
fn launch_effort_suffix_reads_per_tuple_token() {
    let codex_mapping = EffortMapping::new("low", "medium", "xhigh");
    let claude_mapping = EffortMapping::new("low", "medium", "max");
    assert_eq!(
        launch_effort_suffix(EffortLevel::Tough, true, &codex_mapping),
        ":xhigh"
    );
    assert_eq!(
        launch_effort_suffix(EffortLevel::Tough, true, &claude_mapping),
        ":max"
    );
    assert_eq!(
        launch_effort_suffix(EffortLevel::Low, true, &codex_mapping),
        ":low"
    );
}

#[test]
fn launch_effort_suffix_empty_token_is_empty() {
    // Per the design: an empty token yields no suffix even when the
    // candidate is effort-eligible — so a row with an explicitly blank
    // `tough` field never decorates the model name.
    let mapping = EffortMapping::new("", "", "");
    assert_eq!(launch_effort_suffix(EffortLevel::Tough, true, &mapping), "");
}

#[test]
fn run_label_with_model_appends_effort_suffix() {
    let mapping = EffortMapping::new("low", "medium", "xhigh");
    let name = run_label_with_model(
        "[Round 1 Coder]",
        "gpt-5.5",
        EffortLevel::Tough,
        true,
        &mapping,
    );
    assert_eq!(name, "[Round 1 Coder] gpt-5.5:xhigh");
}
