use crate::tasks::Ref;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoderStatus {
    Done,
    Partial,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoderSummary {
    pub status: CoderStatus,
    pub summary: String,
    #[serde(default)]
    pub rebuttal: Vec<String>,
    #[serde(default)]
    pub spec_plan_defect: Vec<SpecPlanDefect>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecPlanDefect {
    pub target: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub defect: String,
    pub fix: String,
}
pub fn validate(path: &Path) -> Result<CoderSummary> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: CoderSummary = toml::from_str(&text)
        .with_context(|| format!("malformed coder summary TOML in {}", path.display()))?;
    if parsed.summary.trim().is_empty() {
        bail!("summary is empty");
    }
    for (i, item) in parsed.rebuttal.iter().enumerate() {
        if item.trim().is_empty() {
            bail!("rebuttal[{i}] is empty");
        }
    }
    for (i, d) in parsed.spec_plan_defect.iter().enumerate() {
        d.validate(&format!("spec_plan_defect[{i}]"))?;
    }
    Ok(parsed)
}
impl SpecPlanDefect {
    pub(crate) fn validate(&self, label: &str) -> Result<()> {
        validate_spec_plan_ref(label, &self.target, &self.r#ref)?;
        if self.defect.trim().is_empty() {
            bail!("{label}: defect is empty");
        }
        if self.fix.trim().is_empty() {
            bail!("{label}: fix is empty");
        }
        Ok(())
    }
}
pub(crate) fn validate_spec_plan_ref(label: &str, target: &str, r: &Ref) -> Result<()> {
    if !matches!(target, "spec" | "plan" | "tasks") {
        bail!(
            "{label}: target must be one of \"spec\", \"plan\", \"tasks\" (got {target:?})"
        );
    }
    match r.path.as_str() {
        "artifacts/spec.md" | "artifacts/plan.md" | "artifacts/tasks.toml" => {}
        other => bail!(
            "{label}: ref.path must be one of \"artifacts/spec.md\", \"artifacts/plan.md\", \"artifacts/tasks.toml\" (got {other:?})"
        ),
    }
    let lines = r.lines.trim();
    if lines.is_empty() {
        bail!("{label}: ref.lines is empty");
    }
    if target == "tasks" {
        // Task id form: "T-<digits>".
        let ok = lines
            .strip_prefix("T-")
            .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false);
        if !ok {
            bail!(
                "{label}: ref.lines must be a task id like \"T-1\" when target=\"tasks\" (got {:?})",
                r.lines
            );
        }
    } else if !is_line_range(lines) {
        bail!(
            "{label}: ref.lines must be a single line \"42\" or range \"42-47\" (got {:?})",
            r.lines
        );
    }
    Ok(())
}
fn is_line_range(s: &str) -> bool {
    let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    if let Some((a, b)) = s.split_once('-') {
        digits(a) && digits(b)
    } else {
        digits(s)
    }
}
#[cfg(test)]
#[path = "coder_summary_tests.rs"]
mod tests;
