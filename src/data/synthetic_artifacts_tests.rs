use super::*;

#[test]
fn generate_synthetic_artifacts_writes_plan_and_tasks() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let spec = Spec {
        content: "spec".to_string(),
        spec_refs: vec!["artifacts/spec.md".to_string()],
    };

    generate_synthetic_artifacts(dir.path(), &spec).expect("generate artifacts");

    let artifacts = dir.path().join("artifacts");
    assert!(artifacts.join("plan.md").exists());
    assert!(artifacts.join("tasks.toml").exists());

    let tasks = std::fs::read_to_string(artifacts.join("tasks.toml")).expect("tasks");
    let parsed: crate::tasks::TasksFile = toml::from_str(&tasks).expect("valid TOML tasks");
    assert_eq!(parsed.tasks.len(), 1);
    assert_eq!(parsed.tasks[0].id, 1);
}
