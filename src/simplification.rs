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
mod tests {
    use super::*;

    fn write_verdict(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("simplification.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn simplified_with_commits_parses() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "simplified"
summary = "Renamed two helpers and inlined a single-use function."
commits = ["abc123", "def456"]
files_touched = ["src/foo.rs", "src/bar.rs"]
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, SimplificationStatus::Simplified);
        assert_eq!(verdict.commits.len(), 2);
        assert_eq!(verdict.files_touched.len(), 2);
    }

    #[test]
    fn no_changes_with_empty_arrays_parses() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "no_changes"
summary = "Diff was already tight; nothing worth touching."
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, SimplificationStatus::NoChanges);
        assert!(verdict.commits.is_empty());
        assert!(verdict.files_touched.is_empty());
    }

    #[test]
    fn skipped_for_docs_only_round_parses() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "skipped"
summary = "Docs-only round; no source changes to simplify."
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, SimplificationStatus::Skipped);
    }

    #[test]
    fn empty_summary_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "no_changes"
summary = "   "
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("summary"));
    }

    #[test]
    fn unknown_status_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "approved"
summary = "wrong status name"
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("malformed simplification TOML"));
    }

    #[test]
    fn empty_commit_sha_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "simplified"
summary = "renamed something"
commits = ["abc", "  "]
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("commits[1]"));
    }

    #[test]
    fn empty_files_touched_path_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "simplified"
summary = "renamed something"
files_touched = ["src/foo.rs", ""]
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("files_touched[1]"));
    }

    #[test]
    fn missing_file_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("cannot read"));
    }

    #[test]
    fn malformed_toml_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is [[[ not valid toml").unwrap();
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("malformed simplification TOML"));
    }
}
