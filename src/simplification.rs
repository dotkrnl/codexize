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
    #[serde(default)]
    pub commits: Vec<String>,
    #[serde(default)]
    pub files_touched: Vec<String>,
}

/// Parse and validate a simplification TOML file written by the simplifier
/// stage. The schema mirrors §2.2 of the spec: `simplified`, `no_changes`,
/// and `skipped` are the only allowed statuses; commits and files_touched
/// are advisory and may be empty.
pub fn validate(path: &Path) -> Result<SimplificationVerdict> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: SimplificationVerdict = toml::from_str(&text)
        .with_context(|| format!("malformed simplification TOML in {}", path.display()))?;

    if parsed.summary.trim().is_empty() {
        bail!("summary is empty");
    }

    for (i, sha) in parsed.commits.iter().enumerate() {
        if sha.trim().is_empty() {
            bail!("commits[{i}]: empty sha");
        }
    }
    for (i, p) in parsed.files_touched.iter().enumerate() {
        if p.trim().is_empty() {
            bail!("files_touched[{i}]: empty path");
        }
    }

    Ok(parsed)
}

#[cfg(test)]
#[path = "simplification_tests.rs"]
mod tests;
