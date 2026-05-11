//! Parser and validator for `artifacts/repo-state-update.toml`.
//!
//! The repo-state update stage writes one of two reports per run:
//!
//! - `status = "implementable"`: both `spec.md` and `plan.md` have been
//!   rewritten; the orchestrator advances the session to `ShardingRunning`
//!   after updating `planned_after_session_id`.
//! - `status = "not_implementable"`: the current idea is no longer
//!   achievable on top of the new repository state; the orchestrator
//!   routes the session to `BlockedNeedsUser`.
//!
//! Both shapes are validated here so the stage driver can match on a
//! typed verdict instead of reaching into raw TOML.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoStateUpdateStatus {
    Implementable,
    NotImplementable,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoStateUpdateBlocker {
    pub description: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RepoStateUpdateReport {
    pub status: RepoStateUpdateStatus,
    pub summary: String,
    pub recorded_baseline: Option<String>,
    pub current_baseline: Option<String>,
    pub git_head: Option<String>,
    pub rewrote_spec: bool,
    pub rewrote_plan: bool,
    pub blockers: Vec<RepoStateUpdateBlocker>,
}

#[derive(Debug, Deserialize)]
struct RawReport {
    status: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    recorded_baseline: Option<String>,
    #[serde(default)]
    current_baseline: Option<String>,
    #[serde(default)]
    git_head: Option<String>,
    #[serde(default)]
    rewrote_spec: Option<bool>,
    #[serde(default)]
    rewrote_plan: Option<bool>,
    #[serde(default)]
    blockers: Vec<RepoStateUpdateBlocker>,
}

pub fn validate(path: &Path) -> Result<RepoStateUpdateReport> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&raw)
}

pub fn parse(toml_str: &str) -> Result<RepoStateUpdateReport> {
    let raw: RawReport =
        toml::from_str(toml_str).with_context(|| "parse repo-state-update.toml")?;
    let summary = raw.summary.trim().to_string();
    if summary.is_empty() {
        bail!("repo-state-update.toml: summary is required and must be non-empty");
    }
    let status = match raw.status.as_str() {
        "implementable" => RepoStateUpdateStatus::Implementable,
        "not_implementable" => RepoStateUpdateStatus::NotImplementable,
        other => bail!(
            "repo-state-update.toml: status must be \"implementable\" or \"not_implementable\", got {other:?}"
        ),
    };
    match status {
        RepoStateUpdateStatus::Implementable => {
            let rewrote_spec = raw.rewrote_spec.unwrap_or(false);
            let rewrote_plan = raw.rewrote_plan.unwrap_or(false);
            if !rewrote_spec || !rewrote_plan {
                bail!(
                    "repo-state-update.toml: status = \"implementable\" requires rewrote_spec = true and rewrote_plan = true"
                );
            }
            if !raw.blockers.is_empty() {
                bail!(
                    "repo-state-update.toml: blockers are forbidden when status = \"implementable\""
                );
            }
        }
        RepoStateUpdateStatus::NotImplementable => {
            if raw.blockers.is_empty() {
                bail!(
                    "repo-state-update.toml: status = \"not_implementable\" requires at least one [[blockers]] entry"
                );
            }
            for (idx, blocker) in raw.blockers.iter().enumerate() {
                if blocker.description.trim().is_empty() {
                    bail!("repo-state-update.toml: blockers[{idx}].description is required");
                }
                if blocker.evidence.is_empty() {
                    bail!(
                        "repo-state-update.toml: blockers[{idx}].evidence must list ≥1 inspected path"
                    );
                }
            }
        }
    }
    Ok(RepoStateUpdateReport {
        status,
        summary,
        recorded_baseline: raw.recorded_baseline.filter(|s| !s.is_empty()),
        current_baseline: raw.current_baseline.filter(|s| !s.is_empty()),
        git_head: raw.git_head.filter(|s| !s.is_empty()),
        rewrote_spec: raw.rewrote_spec.unwrap_or(false),
        rewrote_plan: raw.rewrote_plan.unwrap_or(false),
        blockers: raw.blockers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implementable_with_both_rewrites_parses() {
        let toml_str = r#"
status = "implementable"
summary = "Earlier session refactored the cache; updating plan to match."
recorded_baseline = "20260511-090000-000000001"
current_baseline = "20260511-091000-000000001"
git_head = "abc123"
rewrote_spec = true
rewrote_plan = true
"#;
        let report = parse(toml_str).expect("parse");
        assert_eq!(report.status, RepoStateUpdateStatus::Implementable);
        assert!(report.rewrote_spec && report.rewrote_plan);
        assert!(report.blockers.is_empty());
    }

    #[test]
    fn implementable_requires_both_rewrites() {
        let toml_str = r#"
status = "implementable"
summary = "Partial rewrite — should fail."
rewrote_spec = true
"#;
        let err = parse(toml_str).expect_err("expected failure");
        assert!(
            err.to_string().contains("rewrote_spec") || err.to_string().contains("rewrote_plan"),
            "error should mention required rewrites, got: {err}"
        );
    }

    #[test]
    fn implementable_rejects_blockers() {
        let toml_str = r#"
status = "implementable"
summary = "Ok"
rewrote_spec = true
rewrote_plan = true

[[blockers]]
description = "should not be here"
evidence = ["src/foo.rs"]
"#;
        let err = parse(toml_str).expect_err("expected failure");
        assert!(err.to_string().contains("blockers"));
    }

    #[test]
    fn not_implementable_requires_blockers() {
        let toml_str = r#"
status = "not_implementable"
summary = "Idea no longer applies."
"#;
        let err = parse(toml_str).expect_err("expected failure");
        assert!(err.to_string().contains("blockers"));
    }

    #[test]
    fn not_implementable_blocker_requires_evidence() {
        let toml_str = r#"
status = "not_implementable"
summary = "Idea no longer applies."

[[blockers]]
description = "Earlier session shipped the feature"
"#;
        let err = parse(toml_str).expect_err("expected failure");
        assert!(err.to_string().contains("evidence"));
    }

    #[test]
    fn not_implementable_with_blockers_parses() {
        let toml_str = r#"
status = "not_implementable"
summary = "Already shipped."

[[blockers]]
description = "Earlier session's cache layer already provides the user-visible behavior."
evidence = ["src/cache.rs", "artifacts/spec.md"]
"#;
        let report = parse(toml_str).expect("parse");
        assert_eq!(report.status, RepoStateUpdateStatus::NotImplementable);
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.blockers[0].evidence.len(), 2);
    }

    #[test]
    fn unknown_status_rejected() {
        let toml_str = r#"
status = "maybe"
summary = "..."
"#;
        let err = parse(toml_str).expect_err("expected failure");
        assert!(err.to_string().contains("status"));
    }

    #[test]
    fn missing_summary_rejected() {
        let toml_str = r#"
status = "implementable"
summary = ""
rewrote_spec = true
rewrote_plan = true
"#;
        let err = parse(toml_str).expect_err("expected failure");
        assert!(err.to_string().contains("summary"));
    }
}
