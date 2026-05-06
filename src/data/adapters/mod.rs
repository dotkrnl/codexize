use crate::selection::VendorKind;
use crate::state::LaunchModes;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
#[path = "mod_tests.rs"]
mod tests;
