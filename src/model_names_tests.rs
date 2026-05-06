use super::*;

#[test]
fn display_name_uses_explicit_short_labels() {
    assert_eq!(display_name("gemini-3.1-pro-preview"), "3.1-pro");
    assert_eq!(display_name("gemini-3-pro-preview"), "3-pro");
    assert_eq!(display_name("gemini-3-flash-preview"), "3-flash");
}

#[test]
fn display_name_preserves_unknown_canonical_names() {
    assert_eq!(
        display_name("gemini-custom-preview"),
        "gemini-custom-preview"
    );
}

#[test]
fn display_name_for_vendor_strips_prefix_only_without_explicit_label() {
    assert_eq!(
        display_name_for_vendor("gemini-2.5-pro", "gemini-"),
        "2.5-pro"
    );
    assert_eq!(display_name_for_vendor("gpt-5.2", "gpt-"), "5.2");
}

#[test]
fn run_label_name_keeps_existing_claude_behavior_and_shortens_known_gemini() {
    assert_eq!(run_label_name("claude-sonnet-4.6"), "sonnet-4.6");
    assert_eq!(run_label_name("gemini-3.1-pro-preview"), "3.1-pro");
    assert_eq!(run_label_name("gpt-5.2"), "gpt-5.2");
}
