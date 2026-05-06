
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
fn coder_summary_legacy_dirty_fields_parse() {
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
    assert_eq!(summary.rebuttal.len(), 1);
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
