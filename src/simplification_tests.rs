use super::*;

fn write_verdict(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
    let path = dir.path().join("simplification.toml");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn simplified_parses() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_verdict(
        &dir,
        r#"status = "simplified"
summary = "Renamed two helpers and inlined a single-use function."
"#,
    );
    let verdict = validate(&path).unwrap();
    assert_eq!(verdict.status, SimplificationStatus::Simplified);
}

#[test]
fn no_changes_parses() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_verdict(
        &dir,
        r#"status = "no_changes"
summary = "Diff was already tight; nothing worth touching."
"#,
    );
    let verdict = validate(&path).unwrap();
    assert_eq!(verdict.status, SimplificationStatus::NoChanges);
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
