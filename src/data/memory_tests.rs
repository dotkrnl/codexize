use super::*;

fn write(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn valid_manifest_body() -> String {
    r##"schema_version = 1

[[entries]]
id = "lesson-1"
title = "Use prompt snapshots"
topic = "lessons"
file = "topics/lessons.md"
anchor = "prompt-snapshots"
created_at = "2026-05-06T20:00:00Z"
updated_at = "2026-05-06T20:05:00Z"
last_seen_at = "2026-05-06T20:10:00Z"
last_dreamed_at = "2026-05-06T20:15:00Z"
tier = "hot"
status = "active"
salience = 4
vendors = ["codex"]
paths = ["src/app/prompt_builders.rs"]
supersedes = []
"##
    .to_string()
}

#[test]
fn memory_root_resolves_from_codexize_parent_not_artifact_dir() {
    let path = std::path::Path::new(
        "/repo/.codexize/sessions/20260506-150024/artifacts/final_validation_1.toml",
    );

    assert_eq!(
        memory_root_from_session_path(path),
        std::path::PathBuf::from("/repo/.codexize/memory")
    );
}

#[test]
fn manifest_accepts_valid_metadata_and_targets() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");
    write(&root.join("topics/lessons.md"), "# Lessons\n");
    write(&root.join("manifest.toml"), &valid_manifest_body());

    let manifest = validate_manifest_file(&root.join("manifest.toml")).unwrap();

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.entries[0].tier, MemoryTier::Hot);
    assert_eq!(manifest.entries[0].status, MemoryStatus::Active);
}

#[test]
fn manifest_rejects_missing_target_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");
    write(&root.join("manifest.toml"), &valid_manifest_body());

    let err = validate_manifest_file(&root.join("manifest.toml")).unwrap_err();

    assert!(format!("{err:#}").contains("missing target file"));
}

#[test]
fn manifest_rejects_invalid_salience() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");
    write(&root.join("topics/lessons.md"), "# Lessons\n");
    write(
        &root.join("manifest.toml"),
        &valid_manifest_body().replace("salience = 4", "salience = 6"),
    );

    let err = validate_manifest_file(&root.join("manifest.toml")).unwrap_err();

    assert!(format!("{err:#}").contains("salience"));
}

#[test]
fn manifest_rejects_unknown_supersession_refs() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");
    write(&root.join("topics/lessons.md"), "# Lessons\n");
    write(
        &root.join("manifest.toml"),
        &valid_manifest_body().replace("supersedes = []", r#"supersedes = ["missing"]"#),
    );

    let err = validate_manifest_file(&root.join("manifest.toml")).unwrap_err();

    assert!(format!("{err:#}").contains("unknown supersession"));
}

#[test]
fn manifest_rejects_circular_supersession_refs() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");
    write(&root.join("topics/lessons.md"), "# Lessons\n");
    let mut body = valid_manifest_body();
    body = body.replace("supersedes = []", r#"supersedes = ["lesson-2"]"#);
    body.push_str(
        r#"

[[entries]]
id = "lesson-2"
title = "Second"
topic = "lessons"
file = "topics/lessons.md"
created_at = "2026-05-06T20:00:00Z"
updated_at = "2026-05-06T20:05:00Z"
last_seen_at = "2026-05-06T20:10:00Z"
tier = "warm"
status = "superseded"
salience = 3
supersedes = ["lesson-1"]
"#,
    );
    write(&root.join("manifest.toml"), &body);

    let err = validate_manifest_file(&root.join("manifest.toml")).unwrap_err();

    assert!(format!("{err:#}").contains("circular supersession"));
}

#[test]
fn dream_report_rejects_missing_inputs() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");
    write(
        &root.join("dreams/dream-0001.toml"),
        r#"schema_version = 1
status = "completed"
summary = "Compacted memory."
started_at = "2026-05-06T20:00:00Z"
ended_at = "2026-05-06T20:01:00Z"
inputs = ["index.md"]

[[changes]]
kind = "index_updated"
target = "index.md"
reason = "Kept the index concise."
"#,
    );

    let err = validate_dream_report_file(&root.join("dreams/dream-0001.toml")).unwrap_err();

    assert!(format!("{err:#}").contains("missing input"));
}
