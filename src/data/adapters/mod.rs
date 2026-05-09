use crate::selection::SubscriptionKind;
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
    /// The CLI to spawn for this run. Different from `vendor` for Free
    /// candidates: a Free model may launch through any CLI.
    pub cli: crate::selection::CliKind,
    /// The model string to pass to the CLI verbatim. For Free candidates
    /// this is the operator-supplied `model_name` from config; for direct
    /// and opencode-go candidates this equals `model`.
    pub launch_name: String,
    pub prompt_path: PathBuf,
    pub effort: EffortLevel,
    pub modes: LaunchModes,
}
pub fn all_vendors() -> [SubscriptionKind; 5] {
    [
        SubscriptionKind::Codex,
        SubscriptionKind::Claude,
        SubscriptionKind::Gemini,
        SubscriptionKind::Kimi,
        SubscriptionKind::OpencodeGo,
    ]
}
pub fn short_model(model: &str) -> String {
    crate::model_names::run_label_name(model)
}
pub fn effort_suffix(vendor: SubscriptionKind, effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Normal => "",
        EffortLevel::Low => match vendor {
            SubscriptionKind::Codex | SubscriptionKind::Claude => ":low",
            SubscriptionKind::Gemini
            | SubscriptionKind::Kimi
            | SubscriptionKind::OpencodeGo
            | SubscriptionKind::Direct => "",
        },
        EffortLevel::Tough => match vendor {
            SubscriptionKind::Codex => ":xhigh",
            SubscriptionKind::Claude => ":max",
            SubscriptionKind::Gemini
            | SubscriptionKind::Kimi
            | SubscriptionKind::OpencodeGo
            | SubscriptionKind::Direct => "",
        },
    }
}
pub fn effort_suffix_for_model(
    vendor: SubscriptionKind,
    _model: &str,
    effort: EffortLevel,
) -> &'static str {
    // OpencodeGo routing originally inferred an "underlying" vendor from
    // the model name to pick a suffix; with the heuristic gone, fall back
    // to the routing vendor directly. Per-tuple effort flags will land in
    // a later task.
    effort_suffix(vendor, effort)
}
pub fn effort_suffix_from_str(vendor_str: &str, effort: EffortLevel) -> &'static str {
    match crate::logic::selection::assemble::parse_subscription_str(vendor_str) {
        Some(vendor) => effort_suffix(vendor, effort),
        None => "",
    }
}
pub fn run_label_with_model(
    base: &str,
    model: &str,
    vendor: SubscriptionKind,
    effort: EffortLevel,
) -> String {
    let short = short_model(model);
    let suffix = effort_suffix_for_model(vendor, model, effort);
    if suffix.is_empty() {
        format!("{base} {short}")
    } else {
        format!("{base} {short}{suffix}")
    }
}
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
