use crate::tasks::Task;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
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
    if parsed.status == ReviewStatus::Approved && !parsed.new_tasks.is_empty() {
        bail!("status=approved must not include new_tasks");
    }
    if matches!(
        parsed.status,
        ReviewStatus::Revise | ReviewStatus::HumanBlocked | ReviewStatus::AgentPivot
    ) && parsed.feedback.is_empty()
    {
        bail!(
            "status={:?} requires at least one feedback item",
            parsed.status
        );
    }
    // Validate each new_task has the required fields (reuse tasks::validate-like check)
    for (i, t) in parsed.new_tasks.iter().enumerate() {
        if t.title.trim().is_empty() {
            bail!("new_tasks[{i}]: empty title");
        }
        if t.description.trim().is_empty() {
            bail!("new_tasks[{i}]: empty description");
        }
        if t.test.trim().is_empty() {
            bail!("new_tasks[{i}]: empty test");
        }
        if t.estimated_tokens == 0 {
            bail!("new_tasks[{i}]: estimated_tokens must be > 0");
        }
    }

    Ok(parsed)
}
