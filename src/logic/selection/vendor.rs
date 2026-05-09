use super::types::{CachedModel, CliKind, SubscriptionKind};
use crate::dashboard;
/// Vendors that expose a high-reasoning ("tough") mode.
pub fn is_effort_capable(vendor: SubscriptionKind) -> bool {
    matches!(
        vendor,
        SubscriptionKind::Claude | SubscriptionKind::Codex | SubscriptionKind::OpencodeGo
    )
}
/// Models eligible for tough tasks. Combines the selected CLI capability filter
/// with the Claude-tier filter: only opus Claude variants qualify; all
/// Codex models qualify; Kimi and Gemini do not.
///
/// The Claude check is `to_lowercase().contains("opus")` so future opus
/// names (`claude-opus-4-7`, `claude-opus-5`, …) are admitted automatically.
pub fn is_tough_eligible(model: &CachedModel) -> bool {
    if model.selected_cli() == Some(CliKind::Opencode) {
        return false;
    }
    let vendor = route_identity_for_model(model);
    match vendor {
        SubscriptionKind::Claude => model.name.to_lowercase().contains("opus"),
        SubscriptionKind::Codex => true,
        SubscriptionKind::Kimi
        | SubscriptionKind::Gemini
        | SubscriptionKind::OpencodeGo
        | SubscriptionKind::Free => false,
    }
}
/// Models eligible for Cheap mode. This stays parallel to
/// [`is_tough_eligible`] so budget-tier policy is centralized with vendor
/// model-name matching.
pub fn is_cheap_eligible(model: &CachedModel) -> bool {
    let name = model.name.to_lowercase();
    match route_identity_for_model(model) {
        SubscriptionKind::Claude => !name.contains("opus"),
        SubscriptionKind::Kimi | SubscriptionKind::Codex => true,
        SubscriptionKind::Gemini => name.contains("flash") || name.contains("nano"),
        SubscriptionKind::OpencodeGo | SubscriptionKind::Free => true,
    }
}

pub fn route_identity_for_model(model: &CachedModel) -> SubscriptionKind {
    infer_underlying_vendor_from_name(&model.name)
}

pub fn infer_underlying_vendor_from_name(name: &str) -> SubscriptionKind {
    let name = name.to_lowercase();
    if name.contains("claude")
        || name.contains("opus")
        || name.contains("sonnet")
        || name.contains("haiku")
    {
        SubscriptionKind::Claude
    } else if name.contains("gemini") || name.contains("bison") || name.contains("gecko") {
        SubscriptionKind::Gemini
    } else if name.contains("kimi") || name.contains("moonshot") {
        SubscriptionKind::Kimi
    } else {
        SubscriptionKind::Codex
    }
}
pub fn subscription_kind_to_str(v: SubscriptionKind) -> &'static str {
    match v {
        SubscriptionKind::Claude => "claude",
        SubscriptionKind::Codex => "openai",
        SubscriptionKind::Gemini => "google",
        SubscriptionKind::Kimi => "moonshotai",
        SubscriptionKind::OpencodeGo => "opencode-go",
        SubscriptionKind::Free => "free",
    }
}
pub fn vendor_kind_to_str(v: SubscriptionKind) -> &'static str {
    subscription_kind_to_str(v)
}
pub fn str_to_vendor(s: &str) -> Option<SubscriptionKind> {
    match s {
        "claude" => Some(SubscriptionKind::Claude),
        "codex" => Some(SubscriptionKind::Codex),
        "gemini" => Some(SubscriptionKind::Gemini),
        "kimi" => Some(SubscriptionKind::Kimi),
        "opencode" | "opencode-go" => Some(SubscriptionKind::OpencodeGo),
        "free" => Some(SubscriptionKind::Free),
        _ => None,
    }
}
pub fn vendor_for_dashboard_model(model: &dashboard::DashboardModel) -> Option<SubscriptionKind> {
    let name = model.name.as_str();
    let vendor = model.vendor.as_str();
    if vendor == "opencode" {
        return Some(SubscriptionKind::OpencodeGo);
    }
    // Check by model name patterns first
    if name.starts_with("claude-") || name.contains("claude") {
        return Some(SubscriptionKind::Claude);
    }
    if name.starts_with("gpt-")
        || name.starts_with("o1-")
        || name.contains("gpt")
        || name.contains("codex")
    {
        return Some(SubscriptionKind::Codex);
    }
    if name.starts_with("gemini-")
        || name.contains("gemini")
        || name.contains("bison")
        || name.contains("gecko")
    {
        return Some(SubscriptionKind::Gemini);
    }
    if name.starts_with("kimi-") || name.contains("kimi") || name.contains("moonshot") {
        return Some(SubscriptionKind::Kimi);
    }
    // Check by vendor name
    match vendor {
        "anthropic" | "claude" => Some(SubscriptionKind::Claude),
        "openai" | "microsoft" | "azure" => Some(SubscriptionKind::Codex),
        "google" | "deepmind" => Some(SubscriptionKind::Gemini),
        "kimi" | "moonshotai" | "moonshot" => Some(SubscriptionKind::Kimi),
        _ => {
            // Additional heuristics for unknown models
            if name.contains("opus") || name.contains("sonnet") || name.contains("haiku") {
                Some(SubscriptionKind::Claude)
            } else if name.contains("turbo") || name.contains("davinci") || name.contains("curie") {
                Some(SubscriptionKind::Codex)
            } else if name.contains("palm") || name.contains("lamda") || name.contains("bison") {
                Some(SubscriptionKind::Gemini)
            } else {
                // Unknown vendor/model — skip rather than misassign quota
                None
            }
        }
    }
}
#[cfg(test)]
#[path = "vendor_tests.rs"]
mod tests;
