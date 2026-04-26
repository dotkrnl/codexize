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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_review(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("review.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn review_approved_basic() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "approved"
summary = "All changes look good"
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ReviewStatus::Approved);
        assert_eq!(verdict.summary, "All changes look good");
        assert!(verdict.feedback.is_empty());
        assert!(verdict.new_tasks.is_empty());
    }

    #[test]
    fn review_approved_with_advisory_feedback_is_allowed() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "approved"
summary = "Approved with minor notes"
feedback = ["Consider adding more tests in the future"]
"#,
        );
        // approved + feedback is advisory and must not be rejected
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ReviewStatus::Approved);
        assert_eq!(verdict.feedback.len(), 1);
        assert!(verdict.feedback[0].contains("tests"));
    }

    #[test]
    fn review_revise_requires_feedback() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "revise"
summary = "Needs changes"
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("feedback"));
    }

    #[test]
    fn review_revise_with_feedback_passes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "revise"
summary = "Fix the logic"
feedback = ["The loop condition is wrong"]
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ReviewStatus::Revise);
        assert!(!verdict.feedback.is_empty());
    }

    #[test]
    fn review_human_blocked_requires_feedback() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "human_blocked"
summary = "Need human judgment"
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("feedback"));
    }

    #[test]
    fn review_agent_pivot_requires_feedback() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "agent_pivot"
summary = "Agent can fix the direction"
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("feedback"));
    }

    #[test]
    fn review_human_blocked_with_feedback_passes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "human_blocked"
summary = "Spec needs product clarification"
feedback = ["The product direction for feature X is unclear"]
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ReviewStatus::HumanBlocked);
        assert_eq!(verdict.feedback.len(), 1);
    }

    #[test]
    fn review_agent_pivot_with_feedback_passes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "agent_pivot"
summary = "Agent can repair the plan"
feedback = ["Tasks 3 and 4 are in the wrong order"]
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ReviewStatus::AgentPivot);
    }

    #[test]
    fn review_approved_must_not_have_new_tasks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "approved"
summary = "Looks good"

[[new_tasks]]
id = 10
title = "Extra task"
description = "Something extra"
test = "cargo test"
estimated_tokens = 1000
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("approved"));
    }

    #[test]
    fn review_empty_summary_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "approved"
summary = "   "
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("summary"));
    }

    #[test]
    fn review_revise_with_valid_new_tasks_passes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "revise"
summary = "Split this task"
feedback = ["Task is too large"]

[[new_tasks]]
id = 0
title = "Part A"
description = "First half of the work"
test = "cargo test part_a"
estimated_tokens = 5000

[[new_tasks]]
id = 0
title = "Part B"
description = "Second half of the work"
test = "cargo test part_b"
estimated_tokens = 5000
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ReviewStatus::Revise);
        assert_eq!(verdict.new_tasks.len(), 2);
        assert_eq!(verdict.new_tasks[0].title, "Part A");
    }

    #[test]
    fn review_new_task_missing_title_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "revise"
summary = "Split"
feedback = ["needs splitting"]

[[new_tasks]]
id = 0
title = ""
description = "desc"
test = "cargo test"
estimated_tokens = 1000
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("empty title"));
    }

    #[test]
    fn review_new_task_zero_tokens_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_review(
            &dir,
            r#"status = "revise"
summary = "Split"
feedback = ["needs splitting"]

[[new_tasks]]
id = 0
title = "Task"
description = "desc"
test = "cargo test"
estimated_tokens = 0
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("estimated_tokens"));
    }

    #[test]
    fn review_missing_file_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("cannot read"));
    }

    #[test]
    fn review_malformed_toml_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is [[[ not valid toml").unwrap();
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("malformed review TOML"));
    }

    #[test]
    fn review_all_status_values_roundtrip() {
        for (toml_val, expected) in [
            ("\"approved\"", ReviewStatus::Approved),
            ("\"revise\"", ReviewStatus::Revise),
            ("\"human_blocked\"", ReviewStatus::HumanBlocked),
            ("\"agent_pivot\"", ReviewStatus::AgentPivot),
        ] {
            let status: ReviewStatus = toml::from_str(&format!("status = {toml_val}\n"))
                .map(|w: std::collections::HashMap<String, ReviewStatus>| w["status"].clone())
                .unwrap();
            assert_eq!(status, expected);
        }
    }
}
