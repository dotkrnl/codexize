use crate::state::LaunchModes;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::selection::VendorKind;

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

pub fn all_vendors() -> [VendorKind; 4] {
    [
        VendorKind::Codex,
        VendorKind::Claude,
        VendorKind::Gemini,
        VendorKind::Kimi,
    ]
}

pub fn short_model(model: &str) -> String {
    crate::model_names::run_label_name(model)
}

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

pub fn effort_suffix_from_str(vendor_str: &str, effort: EffortLevel) -> &'static str {
    match crate::selection::vendor::str_to_vendor(vendor_str) {
        Some(vendor) => effort_suffix(vendor, effort),
        None => "",
    }
}

pub fn run_label_with_model(
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
    fn run_label_with_model_appends_effort_suffix() {
        let name = run_label_with_model(
            "[Round 1 Coder]",
            "gpt-5.5",
            VendorKind::Codex,
            EffortLevel::Tough,
        );
        assert_eq!(name, "[Round 1 Coder] gpt-5.5:xhigh");
    }
}
