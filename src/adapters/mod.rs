use crate::model_names;
use crate::selection::VendorKind;
use std::path::PathBuf;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use kimi::KimiAdapter;

pub struct AgentRun {
    pub model: String,
    pub prompt_path: PathBuf,
}

pub trait AgentAdapter: Send + Sync {
    fn detect(&self) -> bool;
    fn interactive_command(&self, model: &str, prompt_path: &str) -> String;
    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String;
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
/// The base (including brackets) is preserved verbatim as a prefix so kill /
/// lookup paths can match by base.
pub fn window_name_with_model(base: &str, model: &str) -> String {
    format!("{base} {}", short_model(model))
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
}
