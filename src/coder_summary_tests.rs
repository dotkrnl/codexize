use super::*;

fn write_summary(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
    let path = dir.path().join("coder_summary.toml");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn coder_summary_done_new_schema_passes_validation() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "Task already complete"
rebuttal = ["[Round 1, Item 2] Already addressed in the latest diff."]
"#,
    );
    let summary = validate(&path).unwrap();
    assert_eq!(summary.status, CoderStatus::Done);
    assert_eq!(summary.rebuttal.len(), 1);
}

#[test]
fn coder_summary_tolerates_unknown_fields() {
    // `deny_unknown_fields` was lifted so the schema can accept future
    // additions (e.g., the `spec_plan_defect` extension) without breaking
    // older readers; verify forward-compat by parsing an unknown key.
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "Task already complete"
dirty_before = false
rebuttal = ["[Round 1, Item 2] Already addressed in the latest diff."]
"#,
    );
    let summary = validate(&path).unwrap();
    assert_eq!(summary.status, CoderStatus::Done);
}

#[test]
fn coder_summary_spec_plan_defect_absent_defaults_to_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"
"#,
    );
    let s = validate(&path).unwrap();
    assert!(s.spec_plan_defect.is_empty());
}

#[test]
fn coder_summary_spec_plan_defect_valid_parses() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "Done with one defect flagged"

[[spec_plan_defect]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "42-47" }
defect = "Section conflates two requirements."
fix = "Split into two bullets, one per requirement."

[[spec_plan_defect]]
target = "tasks"
ref = { path = "artifacts/tasks.toml", lines = "T-3" }
defect = "Task 3 references the wrong plan line."
fix = "Repoint plan_refs to lines 88-99."
"#,
    );
    let s = validate(&path).unwrap();
    assert_eq!(s.spec_plan_defect.len(), 2);
    assert_eq!(s.spec_plan_defect[0].target, "spec");
    assert_eq!(s.spec_plan_defect[1].r#ref.lines, "T-3");
}

#[test]
fn coder_summary_spec_plan_defect_rejects_bad_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"

[[spec_plan_defect]]
target = "code"
ref = { path = "artifacts/spec.md", lines = "1-2" }
defect = "x"
fix = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("target must be one of"));
}

#[test]
fn coder_summary_spec_plan_defect_rejects_unknown_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"

[[spec_plan_defect]]
target = "spec"
ref = { path = "artifacts/other.md", lines = "1-2" }
defect = "x"
fix = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("ref.path must be one of"));
}

#[test]
fn coder_summary_spec_plan_defect_rejects_empty_defect_or_fix() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"

[[spec_plan_defect]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "1" }
defect = "  "
fix = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("defect is empty"));

    let path2 = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"

[[spec_plan_defect]]
target = "spec"
ref = { path = "artifacts/spec.md", lines = "1" }
defect = "x"
fix = ""
"#,
    );
    let err = validate(&path2).unwrap_err();
    assert!(format!("{err:#}").contains("fix is empty"));
}

#[test]
fn coder_summary_spec_plan_defect_rejects_bad_lines() {
    let dir = tempfile::TempDir::new().unwrap();
    // Plain line ref must be digits or digits-digits, not a task id.
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"

[[spec_plan_defect]]
target = "plan"
ref = { path = "artifacts/plan.md", lines = "T-1" }
defect = "x"
fix = "y"
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("ref.lines must be a single line"));

    // Tasks target requires "T-<digits>", not raw line numbers.
    let path2 = write_summary(
        &dir,
        r#"status = "done"
summary = "ok"

[[spec_plan_defect]]
target = "tasks"
ref = { path = "artifacts/tasks.toml", lines = "42-47" }
defect = "x"
fix = "y"
"#,
    );
    let err = validate(&path2).unwrap_err();
    assert!(format!("{err:#}").contains("task id"));
}

#[test]
fn coder_summary_empty_summary_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_summary(
        &dir,
        r#"status = "done"
summary = "   "
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
rebuttal = ["  "]
"#,
    );
    let err = validate(&path).unwrap_err();
    assert!(format!("{err:#}").contains("rebuttal[0] is empty"));
}
