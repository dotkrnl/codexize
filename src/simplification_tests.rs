use super::*;

fn write_verdict(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
    let path = dir.path().join("simplification.toml");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn valid_statuses_parse() {
    for (raw, status) in [
        ("simplified", SimplificationStatus::Simplified),
        ("no_changes", SimplificationStatus::NoChanges),
        ("skipped", SimplificationStatus::Skipped),
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_verdict(
            &dir,
            &format!("status = \"{raw}\"\nsummary = \"Simplification summary.\"\n"),
        );
        let verdict = validate(&path).unwrap();
        assert_eq!(verdict.status, status);
    }
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
