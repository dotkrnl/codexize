use super::*;

#[test]
fn display_vendor_returns_curated_brand_for_known_canonical() {
    assert_eq!(display_vendor("claude-opus-4.7"), Some("claude"));
    assert_eq!(display_vendor("gpt-5.4"), Some("gpt"));
    assert_eq!(display_vendor("gemini-3.1-pro-preview"), Some("gemini"));
    assert_eq!(display_vendor("deepseek-v4-flash"), Some("deepseek"));
    assert_eq!(display_vendor("qwen3.6-plus"), Some("qwen"));
    assert_eq!(display_vendor("kimi-k2.6"), Some("kimi"));
}

#[test]
fn display_vendor_curates_dashboard_only_rows() {
    assert_eq!(display_vendor("kimi-k2-0905"), Some("kimi"));
    assert_eq!(display_vendor("kimi-k2.5"), Some("kimi"));
    assert_eq!(display_vendor("grok-4-latest"), Some("grok"));
    assert_eq!(display_vendor("grok-code-fast-1"), Some("grok"));
    assert_eq!(display_vendor("glm-4.6"), Some("glm"));
    assert_eq!(display_vendor("glm-4.7"), Some("glm"));
}

#[test]
fn display_vendor_returns_none_for_uncurated_canonical() {
    assert_eq!(display_vendor("totally-unknown-model"), None);
}

#[test]
fn every_baked_model_has_curated_display_vendor() {
    for row in crate::logic::selection::baked::BAKED_TABLE {
        assert_ne!(
            display_vendor(row.model),
            None,
            "baked model {} must have a curated display vendor",
            row.model
        );
    }
}

#[test]
fn display_short_returns_curated_short_for_known_canonical() {
    assert_eq!(display_short("claude-opus-4.7"), Some("opus 4.7"));
    assert_eq!(display_short("gpt-5.3-codex"), Some("5.3 codex"));
    assert_eq!(display_short("gemini-3.1-pro-preview"), Some("3.1 pro"));
    assert_eq!(display_short("mimo-v2.5-pro"), Some("v2.5 pro"));
    assert_eq!(display_short("glm-5.1"), Some("5.1"));
    assert_eq!(display_short("grok-code-fast-1"), Some("code fast 1"));
}

#[test]
fn display_short_returns_none_for_uncurated_model() {
    assert_eq!(display_short("totally-unknown-model"), None);
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
