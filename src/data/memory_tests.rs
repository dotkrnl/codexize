use super::*;

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn valid_manifest_body() -> String {
    r#"schema_version = 1

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
"#
    .to_string()
}

#[test]
fn memory_root_resolves_from_codexize_parent_not_artifact_dir() {
    let path =
        Path::new("/repo/.codexize/sessions/20260506-150024/artifacts/final_validation_1.toml");

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
fn ensure_memory_bootstrap_seeds_index_and_manifest_idempotently() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join(".codexize/memory");

    ensure_memory_bootstrap(&root).unwrap();
    assert!(root.join("index.md").is_file());
    let manifest = validate_manifest_file(&root.join("manifest.toml")).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert!(manifest.entries.is_empty());

    // Hand-edit the seed and ensure a second call leaves it alone.
    fs::write(root.join("index.md"), "# Memory\n\n- existing entry\n").unwrap();
    ensure_memory_bootstrap(&root).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("index.md")).unwrap(),
        "# Memory\n\n- existing entry\n"
    );
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

#[test]
fn prune_journal_entries_keeps_recent_and_drops_old() {
    use chrono::{Datelike, Utc};
    let dir = tempfile::TempDir::new().unwrap();
    let memory_root = dir.path().join(".codexize/memory");
    let journal = memory_root.join("journal");
    fs::create_dir_all(&journal).unwrap();

    let now = Utc::now();
    let now_index = (now.year() as i64) * 12 + (now.month() as i64 - 1);
    let stem = |idx: i64| {
        let year = (idx.div_euclid(12)) as i32;
        let month = (idx.rem_euclid(12) + 1) as u32;
        format!("{year:04}-{month:02}")
    };

    let recent = journal.join(format!("{}.md", stem(now_index)));
    let edge = journal.join(format!("{}.md", stem(now_index - 11)));
    let old = journal.join(format!("{}.md", stem(now_index - 12)));
    let very_old = journal.join(format!("{}.md", stem(now_index - 24)));
    let manual_note = journal.join("notes.md");
    write(&recent, "# now\n");
    write(&edge, "# 11 months ago\n");
    write(&old, "# 12 months ago\n");
    write(&very_old, "# 24 months ago\n");
    write(&manual_note, "# operator note\n");

    let pruned = prune_journal_entries(&memory_root, 12).unwrap();

    assert_eq!(
        pruned, 2,
        "12-mo retention drops the two strictly-older files"
    );
    assert!(recent.exists());
    assert!(edge.exists(), "the cutoff month is preserved");
    assert!(!old.exists());
    assert!(!very_old.exists());
    assert!(
        manual_note.exists(),
        "non-YYYY-MM entries are preserved by the prune helper"
    );
}

#[test]
fn prune_journal_entries_noops_when_directory_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let memory_root = dir.path().join(".codexize/memory");
    fs::create_dir_all(&memory_root).unwrap();
    // No journal/ subdir; the helper must not error and must not create
    // any directory of its own.
    let pruned = prune_journal_entries(&memory_root, 6).unwrap();
    assert_eq!(pruned, 0);
    assert!(!memory_root.join("journal").exists());
}

#[test]
fn prune_journal_entries_skips_when_retention_is_zero() {
    let dir = tempfile::TempDir::new().unwrap();
    let memory_root = dir.path().join(".codexize/memory");
    let journal = memory_root.join("journal");
    fs::create_dir_all(&journal).unwrap();
    let ancient = journal.join("1970-01.md");
    write(&ancient, "# pin\n");

    let pruned = prune_journal_entries(&memory_root, 0).unwrap();

    assert_eq!(pruned, 0);
    assert!(
        ancient.exists(),
        "retention=0 must not erase the operator's lessons"
    );
}
