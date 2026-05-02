use crate::tasks::{Ref, Task};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    GoalMet,
    GoalGap,
    NeedsHuman,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub description: String,
    pub checked: Vec<String>,
}

/// Minimal task schema emitted by the validator in a `goal_gap` verdict.
/// Intentionally omits `id`, `spec_refs`, and `plan_refs` — the orchestrator
/// assigns those during ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorGapTask {
    pub title: String,
    pub description: String,
    pub test: String,
    pub estimated_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationVerdict {
    pub status: ValidationStatus,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub gaps: Vec<Gap>,
    #[serde(default)]
    pub new_tasks: Vec<ValidatorGapTask>,
}

/// Parse and validate a final-validation TOML file.
pub fn validate(path: &Path) -> Result<ValidationVerdict> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: ValidationVerdict = toml::from_str(&text)
        .with_context(|| format!("malformed validation TOML in {}", path.display()))?;

    if parsed.summary.trim().is_empty() {
        bail!("summary is empty");
    }

    match parsed.status {
        ValidationStatus::GoalMet => {
            if !parsed.gaps.is_empty() {
                bail!("status=goal_met must not include gaps");
            }
            if !parsed.new_tasks.is_empty() {
                bail!("status=goal_met must not include new_tasks");
            }
        }
        ValidationStatus::GoalGap => {
            if parsed.gaps.is_empty() {
                bail!("status=goal_gap requires at least one gap");
            }
            if parsed.new_tasks.is_empty() {
                bail!("status=goal_gap requires at least one new_task");
            }
        }
        ValidationStatus::NeedsHuman => {
            if parsed.gaps.is_empty() {
                bail!("status=needs_human requires at least one gap");
            }
            if !parsed.new_tasks.is_empty() {
                bail!("status=needs_human must not include new_tasks");
            }
        }
    }

    for (i, gap) in parsed.gaps.iter().enumerate() {
        if gap.description.trim().is_empty() {
            bail!("gaps[{i}]: empty description");
        }
        if gap.checked.is_empty() {
            bail!("gaps[{i}]: checked must not be empty");
        }
        for (j, checked) in gap.checked.iter().enumerate() {
            if checked.trim().is_empty() {
                bail!("gaps[{i}].checked[{j}]: empty citation");
            }
        }
    }

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

/// Convert validator gap tasks into full [`Task`] entries.
///
/// `max_task_id` is the current highest task ID in the session; new IDs start
/// at `max_task_id + 1`.  Each task receives conservative references to
/// `artifacts/spec.md` and the validation verdict artifact so downstream
/// coders have something to anchor on even when the validator did not supply
/// explicit refs.
pub fn normalize_gap_tasks(
    gap_tasks: Vec<ValidatorGapTask>,
    max_task_id: u32,
    verdict_artifact_path: &str,
) -> Vec<Task> {
    let mut next_id = max_task_id + 1;
    gap_tasks
        .into_iter()
        .map(|gt| {
            let id = next_id;
            next_id += 1;
            Task {
                id,
                title: gt.title,
                description: gt.description,
                test: gt.test,
                estimated_tokens: gt.estimated_tokens,
                tough: false,
                spec_refs: vec![
                    Ref {
                        path: "artifacts/spec.md".to_string(),
                        lines: "1-".to_string(),
                    },
                    Ref {
                        path: verdict_artifact_path.to_string(),
                        lines: "1-".to_string(),
                    },
                ],
                plan_refs: vec![],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_verdict(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("final_validation.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    // ------------------------------------------------------------------
    // Valid combinations
    // ------------------------------------------------------------------

    #[test]
    fn goal_met_basic() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_met"
summary = "All goals achieved"
findings = ["Inspected src/ and tests/ — everything looks good"]
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ValidationStatus::GoalMet);
        assert_eq!(verdict.summary, "All goals achieved");
        assert_eq!(verdict.findings.len(), 1);
        assert!(verdict.gaps.is_empty());
        assert!(verdict.new_tasks.is_empty());
    }

    #[test]
    fn goal_gap_with_gaps_and_tasks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Missing error handling"
findings = ["Checked src/errors.rs — gaps found"]

[[gaps]]
description = "No retry logic in the client"
checked = ["src/client.rs"]

[[new_tasks]]
title = "Add retry logic"
description = "Wire exponential backoff into the HTTP client"
test = "cargo test retry::"
estimated_tokens = 5000
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ValidationStatus::GoalGap);
        assert_eq!(verdict.gaps.len(), 1);
        assert_eq!(verdict.new_tasks.len(), 1);
        assert_eq!(verdict.new_tasks[0].title, "Add retry logic");
    }

    #[test]
    fn needs_human_with_gaps() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "needs_human"
summary = "Ambiguous product direction"
findings = ["Workspace status is clean", "Ambiguity in spec section 3"]

[[gaps]]
description = "Operator must decide between A and B"
checked = ["artifacts/spec.md", "src/main.rs"]
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ValidationStatus::NeedsHuman);
        assert_eq!(verdict.gaps.len(), 1);
        assert!(verdict.new_tasks.is_empty());
    }

    #[test]
    fn goal_met_with_empty_findings_allowed() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_met"
summary = "Nothing to validate"
"#,
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, ValidationStatus::GoalMet);
        assert!(verdict.findings.is_empty());
    }

    #[test]
    fn all_status_values_roundtrip() {
        for (toml_val, expected) in [
            ("\"goal_met\"", ValidationStatus::GoalMet),
            ("\"goal_gap\"", ValidationStatus::GoalGap),
            ("\"needs_human\"", ValidationStatus::NeedsHuman),
        ] {
            let status: ValidationStatus = toml::from_str(&format!("status = {toml_val}\n"))
                .map(|w: std::collections::HashMap<String, ValidationStatus>| w["status"].clone())
                .unwrap();
            assert_eq!(status, expected);
        }
    }

    // ------------------------------------------------------------------
    // Invalid combinations
    // ------------------------------------------------------------------

    #[test]
    fn empty_summary_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_met"
summary = "   "
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("summary"));
    }

    #[test]
    fn goal_met_with_gaps_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_met"
summary = "Looks good"

[[gaps]]
description = "Oops"
checked = ["src/a.rs"]
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("goal_met"));
    }

    #[test]
    fn goal_met_with_new_tasks_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_met"
summary = "Looks good"

[[new_tasks]]
title = "Extra"
description = "More"
test = "t"
estimated_tokens = 100
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("goal_met"));
    }

    #[test]
    fn goal_gap_without_gaps_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Missing stuff"

[[new_tasks]]
title = "Fix it"
description = "Do the thing"
test = "cargo test"
estimated_tokens = 1000
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("gap"));
    }

    #[test]
    fn goal_gap_without_new_tasks_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Missing stuff"

[[gaps]]
description = "No retry logic"
checked = ["src/client.rs"]
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("new_task"));
    }

    #[test]
    fn needs_human_without_gaps_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "needs_human"
summary = "Need operator"
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("gap"));
    }

    #[test]
    fn needs_human_with_new_tasks_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "needs_human"
summary = "Need operator"

[[gaps]]
description = "Ambiguous"
checked = ["spec.md"]

[[new_tasks]]
title = "Extra"
description = "More"
test = "t"
estimated_tokens = 100
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("needs_human"));
    }

    #[test]
    fn missing_gap_citations_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Gaps found"

[[gaps]]
description = "Something is wrong"
checked = []

[[new_tasks]]
title = "Fix"
description = "Fix it"
test = "cargo test"
estimated_tokens = 1000
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("checked"));
    }

    #[test]
    fn empty_gap_description_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Gaps found"

[[gaps]]
description = "   "
checked = ["src/a.rs"]

[[new_tasks]]
title = "Fix"
description = "Fix it"
test = "cargo test"
estimated_tokens = 1000
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("description"));
    }

    #[test]
    fn empty_citation_entry_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Gaps found"

[[gaps]]
description = "Something is wrong"
checked = ["src/a.rs", "  "]

[[new_tasks]]
title = "Fix"
description = "Fix it"
test = "cargo test"
estimated_tokens = 1000
"#,
        );
        let err = validate(&path).unwrap_err();
        assert!(format!("{err:#}").contains("citation"));
    }

    #[test]
    fn new_task_empty_title_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Gaps found"

[[gaps]]
description = "Missing foo"
checked = ["src/foo.rs"]

[[new_tasks]]
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
    fn new_task_zero_tokens_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            r#"status = "goal_gap"
summary = "Gaps found"

[[gaps]]
description = "Missing foo"
checked = ["src/foo.rs"]

[[new_tasks]]
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
        assert!(format!("{err:#}").contains("malformed validation TOML"));
    }

    // ------------------------------------------------------------------
    // Normalization
    // ------------------------------------------------------------------

    #[test]
    fn normalize_gap_tasks_assigns_monotonic_ids() {
        let gap_tasks = vec![
            ValidatorGapTask {
                title: "First".to_string(),
                description: "Do first thing".to_string(),
                test: "cargo test first".to_string(),
                estimated_tokens: 1000,
            },
            ValidatorGapTask {
                title: "Second".to_string(),
                description: "Do second thing".to_string(),
                test: "cargo test second".to_string(),
                estimated_tokens: 2000,
            },
        ];
        let tasks = normalize_gap_tasks(gap_tasks, 5, "artifacts/final_validation_1.toml");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, 6);
        assert_eq!(tasks[1].id, 7);
    }

    #[test]
    fn normalize_gap_tasks_from_zero_max() {
        let gap_tasks = vec![ValidatorGapTask {
            title: "Only".to_string(),
            description: "Do it".to_string(),
            test: "cargo test".to_string(),
            estimated_tokens: 100,
        }];
        let tasks = normalize_gap_tasks(gap_tasks, 0, "artifacts/final_validation_2.toml");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 1);
    }

    #[test]
    fn normalize_gap_tasks_adds_conservative_refs() {
        let gap_tasks = vec![ValidatorGapTask {
            title: "Fix".to_string(),
            description: "Fix the bug".to_string(),
            test: "cargo test fix".to_string(),
            estimated_tokens: 500,
        }];
        let tasks = normalize_gap_tasks(gap_tasks, 3, "artifacts/final_validation_1.toml");
        assert_eq!(tasks[0].spec_refs.len(), 2);
        assert_eq!(tasks[0].spec_refs[0].path, "artifacts/spec.md");
        assert_eq!(
            tasks[0].spec_refs[1].path,
            "artifacts/final_validation_1.toml"
        );
        assert!(tasks[0].plan_refs.is_empty());
    }

    #[test]
    fn normalize_gap_tasks_sets_tough_false() {
        let gap_tasks = vec![ValidatorGapTask {
            title: "Fix".to_string(),
            description: "Fix".to_string(),
            test: "t".to_string(),
            estimated_tokens: 100,
        }];
        let tasks = normalize_gap_tasks(gap_tasks, 0, "v.toml");
        assert!(!tasks[0].tough);
    }

    #[test]
    fn normalize_empty_gap_tasks_returns_empty() {
        let tasks: Vec<Task> = normalize_gap_tasks(vec![], 10, "artifacts/fv.toml");
        assert!(tasks.is_empty());
    }
}
