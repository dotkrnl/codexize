use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimplificationStatus {
    /// One or more behavior-preserving edits were committed.
    Simplified,
    /// The simplifier inspected the diff and found nothing worth touching.
    NoChanges,
    /// There was no implementation work to simplify (docs-only round, empty diff).
    Skipped,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplificationVerdict {
    pub status: SimplificationStatus,
    pub summary: String,
}
/// Parse and validate a simplification TOML file written by the simplifier
/// stage. `simplified`, `no_changes`, and `skipped` are the only allowed
/// statuses.
pub fn validate(path: &Path) -> Result<SimplificationVerdict> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: SimplificationVerdict = toml::from_str(&text)
        .with_context(|| format!("malformed simplification TOML in {}", path.display()))?;
    if parsed.summary.trim().is_empty() {
        bail!("summary is empty");
    }
    Ok(parsed)
}
#[cfg(test)]
#[path = "simplification_tests.rs"]
mod tests;
