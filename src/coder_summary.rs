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
    pub dirty_before: bool,
    pub dirty_after: bool,
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
mod tests {
    use super::*;

    fn write_summary(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("coder_summary.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn coder_summary_done_passes_validation() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_summary(
            &dir,
            r#"status = "done"
summary = "Task already complete"
dirty_before = false
dirty_after = true
rebuttal = ["[Round 1, Item 2] Already addressed in the latest diff."]
"#,
        );
        let summary = validate(&path).unwrap();
        assert_eq!(summary.status, CoderStatus::Done);
        assert!(summary.dirty_after);
        assert_eq!(summary.rebuttal.len(), 1);
    }

    #[test]
    fn coder_summary_empty_summary_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_summary(
            &dir,
            r#"status = "done"
summary = "   "
dirty_before = false
dirty_after = false
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("summary is empty"));
    }

    #[test]
    fn coder_summary_empty_rebuttal_item_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_summary(
            &dir,
            r#"status = "partial"
summary = "Need another round"
dirty_before = false
dirty_after = false
rebuttal = ["  "]
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("rebuttal[0] is empty"));
    }
}
