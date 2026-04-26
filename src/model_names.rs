pub const EXPLICIT_SCORE_FALLBACKS: &[(&str, &str)] = &[
    ("gemini-3.1-pro-preview", "gemini-3-pro-preview"),
    ("gemini-3-flash-preview", "gemini-2.5-flash"),
];

pub const GEMINI_KNOWN_QUOTA_MODELS: &[&str] = &[
    "gemini-3.1-pro-preview",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
];

const EXPLICIT_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("gemini-3.1-pro-preview", "3.1-pro"),
    ("gemini-3-pro-preview", "3-pro"),
    ("gemini-3-flash-preview", "3-flash"),
];

pub fn display_name(canonical: &str) -> String {
    explicit_display_name(canonical)
        .unwrap_or(canonical)
        .to_string()
}

pub fn display_name_for_vendor(canonical: &str, vendor_prefix: &str) -> String {
    if let Some(display) = explicit_display_name(canonical) {
        display.to_string()
    } else {
        canonical
            .strip_prefix(vendor_prefix)
            .unwrap_or(canonical)
            .to_string()
    }
}

pub fn tmux_name(canonical: &str) -> String {
    if let Some(display) = explicit_display_name(canonical) {
        display.to_string()
    } else {
        canonical
            .strip_prefix("claude-")
            .unwrap_or(canonical)
            .to_string()
    }
}

fn explicit_display_name(canonical: &str) -> Option<&'static str> {
    EXPLICIT_DISPLAY_NAMES
        .iter()
        .find(|(name, _)| *name == canonical)
        .map(|(_, display)| *display)
}

#[cfg(test)]
mod tests {
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
    fn tmux_name_keeps_existing_claude_behavior_and_shortens_known_gemini() {
        assert_eq!(tmux_name("claude-sonnet-4.6"), "sonnet-4.6");
        assert_eq!(tmux_name("gemini-3.1-pro-preview"), "3.1-pro");
        assert_eq!(tmux_name("gpt-5.2"), "gpt-5.2");
    }
}
