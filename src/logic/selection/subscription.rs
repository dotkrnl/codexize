use super::types::{CachedModel, SubscriptionKind};
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
/// flag).
pub fn is_effort_eligible(model: &CachedModel) -> bool {
    model
        .selected_candidate()
        .is_some_and(|candidate| candidate.effort_eligible)
}

pub fn subscription_kind_to_str(v: SubscriptionKind) -> &'static str {
    match v {
        SubscriptionKind::Claude => "claude",
        SubscriptionKind::Codex => "openai",
        SubscriptionKind::Gemini => "google",
        SubscriptionKind::Kimi => "moonshotai",
        SubscriptionKind::OpencodeGo => "opencode-go",
        SubscriptionKind::Direct => "direct",
    }
}
#[cfg(test)]
#[path = "subscription_tests.rs"]
mod tests;
