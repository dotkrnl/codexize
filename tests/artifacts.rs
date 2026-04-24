use std::fs;
use tempfile::tempdir;

use codexize::artifacts::{ArtifactKind, Implementation, SkipToImplProposal, Spec};
use codexize::synthetic_artifacts::generate_synthetic_artifacts;
use codexize::tasks::TasksFile;

#[test]
fn read_skip_to_impl_proposal_success() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("skip_to_impl.json");
    fs::write(&path, r#"{ "proposed": true, "rationale": "Test rationale" }"#)?;

    let proposal = SkipToImplProposal::read_from_path(&path)?.expect("expected proposal");
    assert!(proposal.proposed);
    assert_eq!(proposal.rationale, "Test rationale");
    Ok(())
}

#[test]
fn read_skip_to_impl_proposal_not_proposed() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("skip_to_impl.json");
    fs::write(&path, r#"{ "proposed": false, "rationale": "" }"#)?;

    let proposal = SkipToImplProposal::read_from_path(&path)?.expect("expected proposal");
    assert!(!proposal.proposed);
    assert_eq!(proposal.rationale, "");
    Ok(())
}

#[test]
fn read_skip_to_impl_proposal_missing_file() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("skip_to_impl.json");
    assert!(SkipToImplProposal::read_from_path(&path)?.is_none());
    Ok(())
}

#[test]
fn read_skip_to_impl_proposal_malformed_json() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_to_impl.json");
    fs::write(&path, r#"{ "proposed": true, "rationale": "#).unwrap();
    assert!(SkipToImplProposal::read_from_path(&path).is_err());
}

#[test]
fn read_skip_to_impl_proposal_empty_rationale_when_proposed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_to_impl.json");
    fs::write(&path, r#"{ "proposed": true, "rationale": "" }"#).unwrap();
    assert!(SkipToImplProposal::read_from_path(&path).is_err());
}

#[test]
fn read_skip_to_impl_proposal_long_rationale() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("skip_to_impl.json");
    let long_rationale = "a".repeat(501);
    let content = format!(r#"{{ "proposed": true, "rationale": "{long_rationale}" }}"#);
    fs::write(&path, content).unwrap();
    assert!(SkipToImplProposal::read_from_path(&path).is_err());
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

    let impl_content = fs::read_to_string(artifacts.join(ArtifactKind::Implementation.filename()))?;
    let implementation: Implementation = serde_json::from_str(&impl_content)?;
    assert_eq!(implementation.current_task_id, 1);
    assert_eq!(implementation.current_round, 1);
    assert_eq!(implementation.remaining_tasks, vec![1]);

    Ok(())
}
