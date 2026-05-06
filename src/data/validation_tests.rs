use super::*;
use crate::tasks::Task;

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
dream_recommendation = "skip"
findings = ["Inspected src/ and tests/ — everything looks good"]
"#,
    );
    let verdict = validate(&path).unwrap();
    assert_eq!(verdict.status, ValidationStatus::GoalMet);
    assert_eq!(verdict.summary, "All goals achieved");
    assert_eq!(verdict.findings.len(), 1);
    assert!(verdict.gaps.is_empty());
    assert!(verdict.new_tasks.is_empty());
    assert_eq!(
        verdict.dream_recommendation,
        Some(DreamRecommendation::Skip)
    );
    assert!(verdict.dream_reason.is_none());
}

#[test]
fn goal_met_with_dream_suggestion_requires_reason() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_verdict(
        &dir,
        r#"status = "goal_met"
summary = "All goals achieved"
findings = ["Inspected src/ and tests/ — everything looks good"]
dream_recommendation = "suggest"
dream_reason = "Several reviewer lessons should be consolidated."
"#,
    );
    let verdict = validate(&path).unwrap();
    assert_eq!(
        verdict.dream_recommendation,
        Some(DreamRecommendation::Suggest)
    );
    assert_eq!(
        verdict.dream_reason.as_deref(),
        Some("Several reviewer lessons should be consolidated.")
    );
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
dream_recommendation = "skip"
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
dream_recommendation = "skip"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("summary"));
}

#[test]
fn goal_met_without_dream_recommendation_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_verdict(
        &dir,
        r#"status = "goal_met"
summary = "Looks good"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("dream_recommendation"));
}

#[test]
fn goal_met_dream_suggest_without_reason_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_verdict(
        &dir,
        r#"status = "goal_met"
summary = "Looks good"
dream_recommendation = "suggest"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("dream_reason"));
}

#[test]
fn goal_met_with_gaps_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_verdict(
        &dir,
        r#"status = "goal_met"
summary = "Looks good"
dream_recommendation = "skip"

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
dream_recommendation = "skip"

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
// Normalization (pure logic, exercised here for legacy continuity)
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
