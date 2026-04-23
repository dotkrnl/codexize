use crate::dashboard;
use super::types::VendorKind;

pub fn str_to_vendor(s: &str) -> Option<VendorKind> {
    match s {
        "claude" => Some(VendorKind::Claude),
        "codex"  => Some(VendorKind::Codex),
        "gemini" => Some(VendorKind::Gemini),
        "kimi"   => Some(VendorKind::Kimi),
        _        => None,
    }
}

pub fn vendor_for_dashboard_model(model: &dashboard::DashboardModel) -> Option<VendorKind> {
    let name = model.name.as_str();
    let vendor = model.vendor.as_str();

    // Check by model name patterns first
    if name.starts_with("claude-") || name.contains("claude") {
        return Some(VendorKind::Claude);
    }
    if name.starts_with("gpt-") || name.starts_with("o1-") || name.contains("gpt") || name.contains("codex") {
        return Some(VendorKind::Codex);
    }
    if name.starts_with("gemini-") || name.contains("gemini") || name.contains("bison") || name.contains("gecko") {
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
