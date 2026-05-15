use crate::data::config::schema::EffortMapping;
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
    /// Per-tuple effort token table (`cheap` / `normal` / `tough`) resolved
    /// from the selected `Candidate` at launch time. Drives
    /// [`launch_effort_suffix`] without consulting any vendor-keyed table.
    pub effort_mapping: EffortMapping,
    /// Whether the selected `Candidate` is effort-capable. When `false`,
    /// [`launch_effort_suffix`] returns an empty string regardless of
    /// `effort`; only the selected candidate's metadata enables suffixes.
    pub effort_eligible: bool,
    pub modes: LaunchModes,
}
pub fn short_model(model: &str) -> String {
    crate::model_names::run_label_name(model)
}
/// Compute the launch-time effort suffix (e.g. `:xhigh`, `:max`) from the
/// selected candidate's per-tuple `effort_mapping` + `effort_eligible`
/// fields. Returns an empty string when the candidate is not effort-capable
/// or `effort` is `Normal`.
pub fn launch_effort_suffix(
    effort: EffortLevel,
    effort_eligible: bool,
    effort_mapping: &EffortMapping,
) -> String {
    let token = match (effort, effort_eligible) {
        (EffortLevel::Low, true) => &effort_mapping.cheap,
        (EffortLevel::Tough, true) => &effort_mapping.tough,
        _ => return String::new(),
    };
    if token.is_empty() {
        String::new()
    } else {
        format!(":{token}")
    }
}
pub fn run_label_with_model(
    base: &str,
    model: &str,
    effort: EffortLevel,
    effort_eligible: bool,
    effort_mapping: &EffortMapping,
) -> String {
    let short = short_model(model);
    let suffix = launch_effort_suffix(effort, effort_eligible, effort_mapping);
    if suffix.is_empty() {
        format!("{base} {short}")
    } else {
        format!("{base} {short}{suffix}")
    }
}
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
