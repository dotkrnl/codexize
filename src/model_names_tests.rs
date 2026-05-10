use super::*;

#[test]
fn display_vendor_returns_curated_brand_for_known_canonical() {
    assert_eq!(display_vendor("claude-opus-4.7"), "claude");
    assert_eq!(display_vendor("gpt-5.4"), "gpt");
    assert_eq!(display_vendor("gemini-3.1-pro-preview"), "gemini");
    assert_eq!(display_vendor("deepseek-v4-flash"), "deepseek");
    assert_eq!(display_vendor("qwen3.6-plus"), "qwen");
    assert_eq!(display_vendor("kimi-k2.6"), "kimi");
}

#[test]
fn display_vendor_curates_dashboard_only_rows() {
    assert_eq!(display_vendor("kimi-k2-0905"), "kimi");
    assert_eq!(display_vendor("kimi-k2.5"), "kimi");
    assert_eq!(display_vendor("grok-4-latest"), "grok");
    assert_eq!(display_vendor("grok-code-fast-1"), "grok");
    assert_eq!(display_vendor("glm-4.6"), "glm");
    assert_eq!(display_vendor("glm-4.7"), "glm");
}

#[test]
fn display_vendor_returns_empty_for_unknown_canonical() {
    assert_eq!(display_vendor("totally-unknown-model"), "");
}

#[test]
fn every_baked_model_has_curated_display_vendor() {
    for row in crate::logic::selection::baked::BAKED_TABLE {
        assert_ne!(
            display_vendor(row.model),
            "",
            "baked model {} must have a curated display vendor",
            row.model
        );
    }
}

#[test]
fn display_short_returns_curated_short_for_known_canonical() {
    assert_eq!(display_short("claude-opus-4.7"), "opus 4.7");
    assert_eq!(display_short("gpt-5.3-codex"), "5.3 codex");
    assert_eq!(display_short("gemini-3.1-pro-preview"), "3.1 preview");
    assert_eq!(display_short("mimo-v2.5-pro"), "v2.5 pro");
    assert_eq!(display_short("glm-5.1"), "5.1");
    assert_eq!(display_short("grok-code-fast-1"), "code fast 1");
}

#[test]
fn display_short_falls_back_to_canonical_for_unknown_model() {
    assert_eq!(
        display_short("totally-unknown-model"),
        "totally-unknown-model"
    );
}

#[test]
fn run_label_name_strips_claude_prefix_and_passes_through_others() {
    assert_eq!(run_label_name("claude-sonnet-4.6"), "sonnet-4.6");
    assert_eq!(run_label_name("gpt-5.4"), "gpt-5.4");
    assert_eq!(
        run_label_name("gemini-3.1-pro-preview"),
        "gemini-3.1-pro-preview"
    );
}
