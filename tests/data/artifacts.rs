use std::fs;
use tempfile::tempdir;

use codexize::artifacts::{ArtifactKind, SkipToImplProposal, Spec};
use codexize::synthetic_artifacts::generate_synthetic_artifacts;
use codexize::tasks::TasksFile;

#[test]
fn read_skip_to_impl_proposal_rejects_json() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_proposal.toml");
    fs::write(&path, r#"{ "proposed": true, "rationale": "old" }"#).unwrap();
    assert!(SkipToImplProposal::read_from_path(&path).is_err());
}

#[test]
fn read_skip_to_impl_proposal_empty_rationale_when_proposed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_proposal.toml");
    fs::write(
        &path,
        "proposed = true\nstatus = \"skip_to_impl\"\nrationale = \"\"\n",
    )
    .unwrap();
    assert!(SkipToImplProposal::read_from_path(&path).is_err());
}

#[test]
fn read_skip_to_impl_proposal_long_rationale_truncates() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_proposal.toml");
    let long_rationale = "a".repeat(600);
    let content =
        format!("proposed = true\nstatus = \"skip_to_impl\"\nrationale = \"{long_rationale}\"\n");
    fs::write(&path, content).unwrap();
    let (proposal, warnings) = SkipToImplProposal::read_from_path(&path).unwrap();
    let proposal = proposal.expect("expected proposal");
    assert_eq!(proposal.rationale.chars().count(), 500);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("truncated to 500 chars (was 600)"));
}

#[test]
fn read_skip_to_impl_proposal_at_500_chars_no_warning() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_proposal.toml");
    let rationale = "b".repeat(500);
    let content =
        format!("proposed = true\nstatus = \"skip_to_impl\"\nrationale = \"{rationale}\"\n");
    fs::write(&path, content).unwrap();
    let (proposal, warnings) = SkipToImplProposal::read_from_path(&path).unwrap();
    let proposal = proposal.expect("expected proposal");
    assert_eq!(proposal.rationale.chars().count(), 500);
    assert!(warnings.is_empty());
}

#[test]
fn read_skip_to_impl_proposal_multibyte_rationale_counts_chars_not_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_proposal.toml");
    // 501 chars but >501 bytes due to multi-byte UTF-8
    let rationale = "\u{00e9}".repeat(501); // é is 2 bytes in UTF-8
    let content =
        format!("proposed = true\nstatus = \"skip_to_impl\"\nrationale = \"{rationale}\"\n");
    fs::write(&path, content).unwrap();
    let (proposal, warnings) = SkipToImplProposal::read_from_path(&path).unwrap();
    let proposal = proposal.expect("expected proposal");
    assert_eq!(proposal.rationale.chars().count(), 500);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("was 501"));
}

#[test]
fn generate_synthetic_artifacts_writes_expected_files() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let session_dir = dir.path().to_path_buf();

    let spec = Spec {
        content: "irrelevant".to_string(),
        spec_refs: vec!["file.md lines 1-10".to_string()],
    };

    generate_synthetic_artifacts(&session_dir, &spec)?;

    let artifacts = session_dir.join("artifacts");

    let plan_content = fs::read_to_string(artifacts.join(ArtifactKind::Plan.filename()))?;
    assert!(plan_content.contains("# Synthetic Plan for Direct Implementation"));
    assert!(plan_content.contains(ArtifactKind::Spec.filename()));

    let tasks_content = fs::read_to_string(artifacts.join(ArtifactKind::Tasks.filename()))?;
    let tasks_file: TasksFile = toml::from_str(&tasks_content)?;
    assert_eq!(tasks_file.tasks.len(), 1);
    assert_eq!(tasks_file.tasks[0].id, 1);
    assert_eq!(tasks_file.tasks[0].title, "Implement according to Spec");
    assert!(!tasks_file.tasks[0].spec_refs.is_empty());
    assert!(!tasks_file.tasks[0].plan_refs.is_empty());

    assert!(!artifacts.join("implementation.json").exists());

    Ok(())
}
