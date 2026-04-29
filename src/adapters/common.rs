use super::EffortLevel;
use crate::model_names;
use crate::selection::VendorKind;
use std::process::Command;

// Shared adapter behavior stops at CLI detection and display/prompt helpers.
// The actual command renderers diverge too far across vendors to justify a
// larger trait without just moving stringly-typed command assembly around.

pub(crate) trait CliBinaryAdapter {
    fn binary_name(&self) -> &'static str;

    fn detect_cli(&self) -> bool {
        Command::new(self.binary_name())
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Short display form of a model name for tmux window titles.
pub fn short_model(model: &str) -> String {
    model_names::tmux_name(model)
}

/// Provider-specific effort suffix for display in tmux window names and TUI labels.
/// Returns an empty string for `Normal` effort and for vendors that do not surface
/// an effort toggle (Gemini, Kimi).
pub fn effort_suffix(vendor: VendorKind, effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Normal => "",
        EffortLevel::Low => match vendor {
            VendorKind::Codex | VendorKind::Claude => ":low",
            VendorKind::Gemini | VendorKind::Kimi => "",
        },
        EffortLevel::Tough => match vendor {
            VendorKind::Codex => ":xhigh",
            VendorKind::Claude => ":max",
            VendorKind::Gemini | VendorKind::Kimi => "",
        },
    }
}

/// Like [`effort_suffix`] but accepts a stored vendor string such as `"codex"` or
/// `"claude"`. Unknown strings (e.g. `"anthropic"`, `"openai"`) produce no suffix.
pub fn effort_suffix_from_str(vendor_str: &str, effort: EffortLevel) -> &'static str {
    match crate::selection::vendor::str_to_vendor(vendor_str) {
        Some(vendor) => effort_suffix(vendor, effort),
        None => "",
    }
}

/// Build a tmux window name that embeds the model, e.g. `[Coder r1] sonnet-4.6`.
/// Appends a provider-specific effort suffix for non-normal effort.
pub fn window_name_with_model(
    base: &str,
    model: &str,
    vendor: VendorKind,
    effort: EffortLevel,
) -> String {
    let short = short_model(model);
    let suffix = effort_suffix(vendor, effort);
    if suffix.is_empty() {
        format!("{base} {short}")
    } else {
        format!("{base} {short}{suffix}")
    }
}

pub(crate) fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || "-_./@=:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub(crate) fn prompt_file_subshell(prompt_path: &str) -> String {
    format!("\"$(cat {})\"", shell_escape(prompt_path))
}

#[cfg(test)]
mod tests {
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
    fn effort_suffix_from_str_converts_known_vendors() {
        assert_eq!(
            effort_suffix_from_str("codex", EffortLevel::Tough),
            ":xhigh"
        );
        assert_eq!(effort_suffix_from_str("claude", EffortLevel::Tough), ":max");
        assert_eq!(effort_suffix_from_str("gemini", EffortLevel::Tough), "");
    }

    #[test]
    fn effort_suffix_from_str_unknown_vendor_returns_empty() {
        assert_eq!(effort_suffix_from_str("anthropic", EffortLevel::Tough), "");
        assert_eq!(effort_suffix_from_str("openai", EffortLevel::Tough), "");
        assert_eq!(effort_suffix_from_str("", EffortLevel::Tough), "");
    }

    #[test]
    fn window_name_with_model_normal_omits_suffix() {
        let name = window_name_with_model(
            "[Round 1 Coder]",
            "claude-sonnet-4.6",
            VendorKind::Claude,
            EffortLevel::Normal,
        );
        assert_eq!(name, "[Round 1 Coder] sonnet-4.6");
    }

    #[test]
    fn window_name_with_model_tough_appends_provider_suffix() {
        let claude = window_name_with_model(
            "[Round 1 Coder]",
            "claude-opus-4-7",
            VendorKind::Claude,
            EffortLevel::Tough,
        );
        assert_eq!(claude, "[Round 1 Coder] opus-4-7:max");

        let codex = window_name_with_model(
            "[Round 1 Coder]",
            "gpt-5.5",
            VendorKind::Codex,
            EffortLevel::Tough,
        );
        assert_eq!(codex, "[Round 1 Coder] gpt-5.5:xhigh");
    }

    #[test]
    fn window_name_with_model_low_appends_low_suffix() {
        let name = window_name_with_model(
            "[Round 1 Coder]",
            "claude-sonnet-4.6",
            VendorKind::Claude,
            EffortLevel::Low,
        );
        assert_eq!(name, "[Round 1 Coder] sonnet-4.6:low");
    }

    #[test]
    fn window_name_with_model_gemini_tough_omits_suffix() {
        let name = window_name_with_model(
            "[Brainstorm]",
            "gemini-3.1-pro-preview",
            VendorKind::Gemini,
            EffortLevel::Tough,
        );
        // Gemini has no effort suffix regardless of effort level.
        assert_eq!(name, "[Brainstorm] 3.1-pro");
    }

    #[test]
    fn prompt_file_subshell_escapes_paths_with_spaces() {
        let subshell = prompt_file_subshell("/tmp/path with spaces/prompt.txt");
        assert_eq!(subshell, r#""$(cat '/tmp/path with spaces/prompt.txt')""#);
    }
}
