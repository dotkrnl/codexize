use crate::coder_summary::validate_spec_plan_ref;
use crate::tasks::{Ref, Task};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
    /// Minor improvements suggested but the current task is accepted as-is.
    /// No re-review of this round; feedback is carried into the next coder
    /// prompt. Use for nice-to-haves that aren't spec/plan violations.
    Refine,
    Revise,
    HumanBlocked,
    AgentPivot,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewVerdict {
    pub status: ReviewStatus,
    pub summary: String,
    #[serde(default)]
    pub feedback: Vec<String>,
    #[serde(default)]
    pub new_tasks: Vec<Task>,
    #[serde(default)]
    pub spec_plan_patch: Vec<SpecPlanPatch>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecPlanPatch {
    pub target: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub defect: String,
    pub patch: String,
}
impl SpecPlanPatch {
    pub(crate) fn validate(&self, label: &str) -> Result<()> {
        validate_spec_plan_ref(label, &self.target, &self.r#ref)?;
        if self.defect.trim().is_empty() {
            bail!("{label}: defect is empty");
        }
        if self.patch.trim().is_empty() {
            bail!("{label}: patch is empty");
        }
        Ok(())
    }
}
/// Parse and validate a review TOML file.
pub fn validate(path: &Path) -> Result<ReviewVerdict> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: ReviewVerdict = toml::from_str(&text)
        .with_context(|| format!("malformed review TOML in {}", path.display()))?;
    if parsed.summary.trim().is_empty() {
        bail!("summary is empty");
    }
    let no_new_tasks_status = match parsed.status {
        ReviewStatus::Approved => Some("approved"),
        ReviewStatus::Refine => Some("refine"),
        _ => None,
    };
    if let Some(status) = no_new_tasks_status
        && !parsed.new_tasks.is_empty()
    {
        bail!("status={status} must not include new_tasks");
    }
    if parsed.status != ReviewStatus::Approved && parsed.feedback.is_empty() {
        bail!(
            "status={:?} requires at least one feedback item",
            parsed.status
        );
    }
    for (i, t) in parsed.new_tasks.iter().enumerate() {
        t.validate_fields(&format!("new_tasks[{i}]"))?;
    }
    for (i, p) in parsed.spec_plan_patch.iter().enumerate() {
        p.validate(&format!("spec_plan_patch[{i}]"))?;
    }
    Ok(parsed)
}
impl ReviewVerdict {
    /// Reject `refine` on the round's terminal review task. `refine` only
    /// has a downstream consumer when another coder run follows (carryover
    /// is drained at the start of the next coder); on the last task that
    /// channel is gone, so refine is silently dropped in YOLO mode (which
    /// transitions straight to Done) or only opportunistically applied by
    /// the simplifier. Either way, an operator-relevant suggestion can
    /// disappear into the void — force the reviewer to commit to
    /// `approved` (no carryover) or `revise` (re-run the task) instead.
    pub fn enforce_terminal_review(&self, is_terminal: bool) -> Result<()> {
        if is_terminal && self.status == ReviewStatus::Refine {
            bail!(
                "status=refine is not allowed on the round's last reviewable task: refine carryover would be dropped (YOLO) or only opportunistically applied by the simplifier; use status=approved or status=revise instead"
            );
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
