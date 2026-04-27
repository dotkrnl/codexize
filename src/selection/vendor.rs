use super::types::{CachedModel, VendorKind};
use crate::dashboard;

/// Vendors that expose a high-reasoning ("tough") mode.
pub fn is_effort_capable(vendor: VendorKind) -> bool {
    matches!(vendor, VendorKind::Claude | VendorKind::Codex)
}

/// Models eligible for tough tasks. Combines the vendor-capability filter
/// with the Claude-tier filter: only opus Claude variants qualify; all
/// Codex models qualify; Kimi and Gemini do not.
///
/// The Claude check is `to_lowercase().contains("opus")` so future opus
/// names (`claude-opus-4-7`, `claude-opus-5`, …) are admitted automatically.
pub fn is_tough_eligible(model: &CachedModel) -> bool {
    match model.vendor {
        VendorKind::Claude => model.name.to_lowercase().contains("opus"),
        VendorKind::Codex => true,
        VendorKind::Kimi | VendorKind::Gemini => false,
    }
}

pub fn vendor_kind_to_str(v: VendorKind) -> &'static str {
    match v {
        VendorKind::Claude => "claude",
        VendorKind::Codex => "openai",
        VendorKind::Gemini => "google",
        VendorKind::Kimi => "moonshotai",
    }
}

pub fn str_to_vendor(s: &str) -> Option<VendorKind> {
    match s {
        "claude" => Some(VendorKind::Claude),
        "codex" => Some(VendorKind::Codex),
        "gemini" => Some(VendorKind::Gemini),
        "kimi" => Some(VendorKind::Kimi),
        _ => None,
    }
}

pub fn vendor_for_dashboard_model(model: &dashboard::DashboardModel) -> Option<VendorKind> {
    let name = model.name.as_str();
    let vendor = model.vendor.as_str();

    // Check by model name patterns first
    if name.starts_with("claude-") || name.contains("claude") {
        return Some(VendorKind::Claude);
    }
    if name.starts_with("gpt-")
        || name.starts_with("o1-")
        || name.contains("gpt")
        || name.contains("codex")
    {
        return Some(VendorKind::Codex);
    }
    if name.starts_with("gemini-")
        || name.contains("gemini")
        || name.contains("bison")
        || name.contains("gecko")
    {
        return Some(VendorKind::Gemini);
    }
    if name.starts_with("kimi-") || name.contains("kimi") || name.contains("moonshot") {
        return Some(VendorKind::Kimi);
    }

    // Check by vendor name
    match vendor {
        "anthropic" | "claude" => Some(VendorKind::Claude),
        "openai" | "microsoft" | "azure" => Some(VendorKind::Codex),
        "google" | "deepmind" => Some(VendorKind::Gemini),
        "kimi" | "moonshotai" | "moonshot" => Some(VendorKind::Kimi),
        _ => {
            // Additional heuristics for unknown models
            if name.contains("opus") || name.contains("sonnet") || name.contains("haiku") {
                Some(VendorKind::Claude)
            } else if name.contains("turbo") || name.contains("davinci") || name.contains("curie") {
                Some(VendorKind::Codex)
            } else if name.contains("palm") || name.contains("lamda") || name.contains("bison") {
                Some(VendorKind::Gemini)
            } else {
                // Unknown vendor/model — skip rather than misassign quota
                None
            }
        }
    }
}
