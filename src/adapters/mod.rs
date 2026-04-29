use crate::selection::VendorKind;
use crate::state::LaunchModes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

mod common;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub(crate) use common::{CliBinaryAdapter, prompt_file_subshell, shell_escape};
pub use common::{effort_suffix, effort_suffix_from_str, short_model, window_name_with_model};
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

pub fn all_vendors() -> [VendorKind; 4] {
    [
        VendorKind::Codex,
        VendorKind::Claude,
        VendorKind::Gemini,
        VendorKind::Kimi,
    ]
}

pub fn detect_available_vendors() -> BTreeSet<VendorKind> {
    #[cfg(test)]
    if let Ok(raw) = std::env::var("CODEXIZE_TEST_AVAILABLE_VENDORS") {
        return raw
            .split(',')
            .filter_map(|name| match name.trim() {
                "claude" => Some(VendorKind::Claude),
                "codex" | "openai" => Some(VendorKind::Codex),
                "gemini" | "google" => Some(VendorKind::Gemini),
                "kimi" | "moonshotai" => Some(VendorKind::Kimi),
                _ => None,
            })
            .collect();
    }

    all_vendors()
        .into_iter()
        .filter(|vendor| adapter_for_vendor(*vendor).detect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
