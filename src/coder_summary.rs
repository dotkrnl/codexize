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
    Ok(parsed)
}
#[cfg(test)]
#[path = "coder_summary_tests.rs"]
mod tests;
