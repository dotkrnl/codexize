use super::*;

#[test]
fn test_stage_io_lookup() {
    assert!(stage_io("brainstorm").is_some());
    assert!(stage_io("coder").is_some());
    assert!(stage_io("reviewer").is_some());
    assert!(stage_io("recovery").is_some());
    assert!(stage_io("nonexistent").is_none());
}

#[test]
fn test_brainstorm_io_writes_spec() {
    let io = stage_io("brainstorm").unwrap();
    assert!(io.writes.contains(&"artifacts/spec.md"));
}

#[test]
fn test_sharder_io_reads_spec_and_plan() {
    let io = stage_io("sharder").unwrap();
    assert!(io.pointer_artifacts.contains(&"artifacts/spec.md"));
    assert!(io.pointer_artifacts.contains(&"artifacts/plan.md"));
}

#[test]
fn test_coder_io_uses_round_task_artifacts() {
    let io = stage_io("coder").unwrap();
    assert!(io.pointer_artifacts.contains(&"rounds/{round}/task.toml"));
    assert!(io.pointer_artifacts.contains(&"rounds/{round}/review.toml"));
    assert!(io.writes.contains(&"rounds/{round}/coder_summary.toml"));
}

#[test]
fn test_reviewer_io_writes_round_review() {
    let io = stage_io("reviewer").unwrap();
    assert!(io.pointer_artifacts.contains(&"rounds/{round}/task.toml"));
    assert!(
        io.pointer_artifacts
            .contains(&"rounds/{round}/review_scope.toml")
    );
    assert!(
        io.pointer_artifacts
            .contains(&"rounds/{round}/coder_summary.toml")
    );
    assert!(io.writes.contains(&"rounds/{round}/review.toml"));
}

#[test]
fn test_recovery_io_uses_trigger_review_and_writes_recovery() {
    let io = stage_io("recovery").unwrap();
    assert!(io.pointer_artifacts.contains(&"rounds/{round}/review.toml"));
    assert!(io.writes.contains(&"artifacts/spec.md"));
    assert!(io.writes.contains(&"artifacts/plan.md"));
    assert!(io.writes.contains(&"artifacts/tasks.toml"));
    assert!(io.writes.contains(&"rounds/{round}/recovery.toml"));
}

#[test]
fn simplifier_io_lookup_and_paths() {
    let io = stage_io("simplifier").expect("simplifier StageIO is registered");
    assert_eq!(io.stage, "simplifier");
    assert!(io.pointer_artifacts.contains(&"artifacts/spec.md"));
    assert!(
        io.pointer_artifacts
            .contains(&"rounds/{round}/review_scope.toml")
    );
    assert!(io.writes.contains(&"rounds/{round}/simplification.toml"));
}
