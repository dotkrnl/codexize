use crate::model_names;
use crate::selection::VendorKind;
use crate::state::LaunchModes;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use kimi::KimiAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EffortLevel {
    Low,
    #[default]
    Normal,
    Tough,
}

pub struct AgentRun {
    pub model: String,
    pub prompt_path: PathBuf,
    pub effort: EffortLevel,
    pub modes: LaunchModes,
}

pub trait AgentAdapter: Send + Sync {
    fn detect(&self) -> bool;
    fn interactive_command(&self, model: &str, prompt_path: &str, effort: EffortLevel) -> String;
    fn noninteractive_command(&self, model: &str, prompt_path: &str, effort: EffortLevel)
    -> String;
}

pub fn adapter_for_vendor(vendor: VendorKind) -> Box<dyn AgentAdapter> {
    match vendor {
        VendorKind::Codex => Box::new(CodexAdapter),
        VendorKind::Claude => Box::new(ClaudeAdapter),
        VendorKind::Kimi => Box::new(KimiAdapter),
        VendorKind::Gemini => Box::new(GeminiAdapter),
    }
}

/// Short display form of a model name for tmux window titles.
pub fn short_model(model: &str) -> String {
    model_names::tmux_name(model)
}

/// Build a tmux window name that embeds the model, e.g. `[Coder r1] sonnet-4.6`.
/// Appends an effort suffix for non-normal effort.
pub fn window_name_with_model(base: &str, model: &str, effort: EffortLevel) -> String {
    let short = short_model(model);
    match effort {
        EffortLevel::Low => format!("{base} {short} [low]"),
        EffortLevel::Tough => format!("{base} {short} [tough]"),
        EffortLevel::Normal => format!("{base} {short}"),
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
    fn window_name_with_model_normal_omits_suffix() {
        let name =
            window_name_with_model("[Round 1 Coder]", "claude-sonnet-4.6", EffortLevel::Normal);
        assert_eq!(name, "[Round 1 Coder] sonnet-4.6");
    }

    #[test]
    fn window_name_with_model_tough_appends_suffix() {
        let name =
            window_name_with_model("[Round 1 Coder]", "claude-sonnet-4.6", EffortLevel::Tough);
        assert_eq!(name, "[Round 1 Coder] sonnet-4.6 [tough]");
    }

    #[test]
    fn window_name_with_model_low_appends_suffix() {
        let name = window_name_with_model("[Round 1 Coder]", "claude-sonnet-4.6", EffortLevel::Low);
        assert_eq!(name, "[Round 1 Coder] sonnet-4.6 [low]");
    }

    #[test]
    fn adapter_for_vendor_dispatches_each_variant() {
        // Each concrete adapter's `interactive_command` invokes a vendor-specific
        // CLI binary; checking for that substring gives a vendor fingerprint
        // without needing TypeId-based downcasting.
        let pairs = [
            (VendorKind::Claude, "claude "),
            (VendorKind::Codex, "codex "),
            (VendorKind::Gemini, "gemini "),
            (VendorKind::Kimi, "kimi "),
        ];
        for (vendor, marker) in pairs {
            let adapter = adapter_for_vendor(vendor);
            let cmd = adapter.interactive_command("model-x", "/tmp/p", EffortLevel::Normal);
            assert!(
                cmd.contains(marker),
                "{:?} adapter should produce a command containing {:?}, got: {}",
                vendor,
                marker,
                cmd
            );
        }
    }
}
