use super::types::{CachedModel, SubscriptionKind};
use crate::dashboard;
/// Vendors that expose a high-reasoning ("tough") mode.
///
/// Per spec §provider properties, effort-eligibility is intrinsically a
/// per-tuple toggle (see [`is_effort_eligible`]). This vendor-level
/// helper survives only as a coarse fallback for call sites that do not
/// have a `CachedModel` in scope — typically when the row's selected
/// candidate has been computed but the consumer wants a vendor-level
/// "could this subscription ever be effort-capable" answer.
pub fn is_effort_capable(vendor: SubscriptionKind) -> bool {
    matches!(
        vendor,
        SubscriptionKind::Claude | SubscriptionKind::Codex | SubscriptionKind::OpencodeGo
    )
}
/// True when the row's *selected* provider tuple is flagged as
/// tough-eligible in the baked defaults table (or by a user
/// `[[providers]]` override). Returns `false` for rows with no selected
/// candidate, mirroring the legacy "no candidate ⇒ not eligible"
/// behavior.
pub fn is_tough_eligible(model: &CachedModel) -> bool {
    model
        .selected_candidate()
        .is_some_and(|candidate| candidate.tough_eligible)
}
/// True when the row's *selected* provider tuple is flagged as
/// cheap-eligible. Same `selected_candidate`-driven semantics as
/// [`is_tough_eligible`].
pub fn is_cheap_eligible(model: &CachedModel) -> bool {
    model
        .selected_candidate()
        .is_some_and(|candidate| candidate.cheap_eligible)
}
/// True when the row's *selected* provider tuple is flagged as
/// effort-eligible (so the run can hand it a high-reasoning effort
/// flag). Prefer this over [`is_effort_capable`] when a `CachedModel`
/// is in scope — it honors per-tuple operator overrides.
pub fn is_effort_eligible(model: &CachedModel) -> bool {
    model
        .selected_candidate()
        .is_some_and(|candidate| candidate.effort_eligible)
}

/// Heuristic vendor inference from a model-name substring. Selection
/// eligibility no longer reads this — `Candidate` flags drive it now —
/// but `data::adapters` still uses it to route OpencodeGo-routed models
/// back to their underlying vendor's effort-suffix table.
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
pub fn vendor_kind_to_str(v: SubscriptionKind) -> &'static str {
    match v {
        SubscriptionKind::Claude => "claude",
        SubscriptionKind::Codex => "openai",
        SubscriptionKind::Gemini => "google",
        SubscriptionKind::Kimi => "moonshotai",
        SubscriptionKind::OpencodeGo => "opencode-go",
        SubscriptionKind::Free => "free",
    }
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
