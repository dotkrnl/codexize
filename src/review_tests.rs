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
fn review_refine_requires_feedback() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "refine"
summary = "Mostly good"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("feedback"));
}

#[test]
fn review_refine_with_feedback_passes() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "refine"
summary = "Approved with nits"
feedback = ["Rename `foo` to `foo_bar` next time", "Drop the dead import"]
"#,
    );
    let verdict = validate(&path).unwrap();
    assert_eq!(verdict.status, ReviewStatus::Refine);
    assert_eq!(verdict.feedback.len(), 2);
    assert!(verdict.new_tasks.is_empty());
}

#[test]
fn review_refine_must_not_have_new_tasks() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "refine"
summary = "Nits only"
feedback = ["Tighten the names"]

[[new_tasks]]
id = 0
title = "Extra"
description = "More"
test = "cargo test"
estimated_tokens = 100
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("refine"));
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
fn enforce_terminal_review_rejects_refine_on_last_task() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "refine"
summary = "Mostly good"
feedback = ["Tighten the names"]
"#,
    );
    let verdict = validate(&path).unwrap();
    let err = verdict.enforce_terminal_review(true).unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("status=refine is not allowed"),
        "error should explain why refine is forbidden, got: {rendered}"
    );
    assert!(
        rendered.contains("approved") && rendered.contains("revise"),
        "error should point at the allowed alternatives, got: {rendered}"
    );
}

#[test]
fn enforce_terminal_review_allows_refine_when_more_work_remains() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "refine"
summary = "Mostly good"
feedback = ["Tighten the names"]
"#,
    );
    let verdict = validate(&path).unwrap();
    // is_terminal=false → refine is fine; carryover flows to next coder.
    verdict.enforce_terminal_review(false).unwrap();
}

#[test]
fn enforce_terminal_review_allows_approved_and_revise_on_last_task() {
    let dir = tempfile::TempDir::new().unwrap();
    let approved_path = write_review(
        &dir,
        r#"status = "approved"
summary = "Looks good"
"#,
    );
    let approved = validate(&approved_path).unwrap();
    approved.enforce_terminal_review(true).unwrap();

    let revise_path = dir.path().join("revise.toml");
    std::fs::write(
        &revise_path,
        r#"status = "revise"
summary = "Fix the loop"
feedback = ["The loop condition is wrong"]
"#,
    )
    .unwrap();
    let revise = validate(&revise_path).unwrap();
    revise.enforce_terminal_review(true).unwrap();
}

#[test]
fn review_spec_plan_patch_absent_defaults_to_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "approved"
summary = "ok"
"#,
    );
    let v = validate(&path).unwrap();
    assert!(v.spec_plan_patch.is_empty());
}

#[test]
fn review_spec_plan_patch_valid_parses() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "approved"
summary = "Patched spec and tasks"

[[spec_plan_patch]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "42-47" }
defect = "Wording conflated two requirements."
patch = "Split into two bullets."

[[spec_plan_patch]]
target = "tasks"
ref = { path = "artifacts/tasks.toml", lines = "T-3" }
defect = "Task 3 references the wrong plan line."
patch = "Repointed plan_refs to lines 88-99."
"#,
    );
    let v = validate(&path).unwrap();
    assert_eq!(v.spec_plan_patch.len(), 2);
    assert_eq!(v.spec_plan_patch[0].r#ref.lines, "42-47");
    assert_eq!(v.spec_plan_patch[1].target, "tasks");
}

#[test]
fn review_spec_plan_patch_rejects_bad_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "approved"
summary = "ok"

[[spec_plan_patch]]
target = "code"
ref = { path = "artifacts/spec.md", lines = "1-2" }
defect = "x"
patch = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("target must be one of"));
}

#[test]
fn review_spec_plan_patch_rejects_unknown_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "approved"
summary = "ok"

[[spec_plan_patch]]
target = "spec"
ref = { path = "artifacts/other.md", lines = "1-2" }
defect = "x"
patch = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("ref.path must be one of"));
}

#[test]
fn review_spec_plan_patch_rejects_empty_defect_or_patch() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "approved"
summary = "ok"

[[spec_plan_patch]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "1" }
defect = "  "
patch = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("defect is empty"));

    let path2 = write_review(
        &dir,
        r#"status = "approved"
summary = "ok"

[[spec_plan_patch]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "1" }
defect = "x"
patch = ""
"#,
    );
    let err = validate(&path2).unwrap_err();
    assert!(format!("{err:#}").contains("patch is empty"));
}

#[test]
fn review_spec_plan_patch_rejects_bad_lines_for_tasks() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "approved"
summary = "ok"

[[spec_plan_patch]]
target = "tasks"
ref = { path = "artifacts/tasks.toml", lines = "42-47" }
defect = "x"
patch = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("task id"));
}

#[test]
fn review_mismatched_defect_and_patch_lengths_are_permitted() {
    // The reviewer may accept some coder-flagged defects (one patch each)
    // and reject others (those become feedback items). The parser must not
    // require equal cardinality between defect input and patch output.
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_review(
        &dir,
        r#"status = "revise"
summary = "Accepted one, rejected the other"
feedback = ["Second defect was unsound: the section already covers that case"]

[[spec_plan_patch]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "42-47" }
defect = "Wording conflated two requirements."
patch = "Split into two bullets."
"#,
    );
    let v = validate(&path).unwrap();
    assert_eq!(v.spec_plan_patch.len(), 1);
    assert_eq!(v.feedback.len(), 1);
}

#[test]
fn review_all_status_values_roundtrip() {
    for (toml_val, expected) in [
        ("\"approved\"", ReviewStatus::Approved),
        ("\"refine\"", ReviewStatus::Refine),
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
