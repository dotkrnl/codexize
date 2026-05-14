use super::*;

fn warnings_of(result: &VerifyResult) -> Vec<String> {
    match result {
        VerifyResult::Ok { warnings }
        | VerifyResult::HardError { warnings, .. }
        | VerifyResult::PendingDecision { warnings, .. } => warnings.clone(),
    }
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_non_coder_warns_on_pre_dirty_status() {
    let head = git_head().unwrap_or_default();
    let current_status = git_status().unwrap_or_default();
    let snap = test_snapshot(&head, &format!("{current_status} M dirty.txt\n"));
    let result = verify_non_coder(&snap);
    assert!(matches!(result, VerifyResult::Ok { .. }));
    let warnings = warnings_of(&result);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("dirty before agent launch")),
        "expected dirty-tree warning, got: {warnings:?}"
    );
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_non_coder_warns_on_changed_status() {
    let head = git_head().unwrap_or_default();
    let current_status = git_status().unwrap_or_default();
    let snap = test_snapshot(&head, &format!("{current_status}?? phantom-file.xyz\n"));
    let result = verify_non_coder(&snap);
    assert!(matches!(result, VerifyResult::Ok { .. }));
    let warnings = warnings_of(&result);
    assert!(
        warnings.iter().any(|w| w.contains("modified working tree")),
        "expected modified-tree warning, got: {warnings:?}"
    );
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_non_coder_hard_error_on_head_advance_auto_reset() {
    let snap = test_snapshot("0000000000000000000000000000000000000000", "");
    let result = verify_non_coder(&snap);
    match result {
        VerifyResult::HardError { reason, .. } => {
            assert_eq!(reason, "forbidden_head_advance");
        }
        other => panic!("expected HardError, got {other:?}"),
    }
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_non_coder_pending_on_head_advance_ask_operator() {
    let mut snap = test_snapshot("0000000000000000000000000000000000000000", "");
    snap.mode = GuardMode::AskOperator;
    let current = git_head().unwrap_or_default();
    let result = verify_non_coder(&snap);
    match result {
        VerifyResult::PendingDecision {
            captured_head,
            current_head,
            ..
        } => {
            assert_eq!(captured_head, "0000000000000000000000000000000000000000");
            assert_eq!(current_head, current);
            // Confirm we did NOT reset: HEAD must still match what we
            // observed before calling verify. (verify already read it
            // once; if reset had happened, current_head would equal
            // captured_head — which we explicitly assert is not the
            // captured zero-SHA above.)
            assert_ne!(current_head, captured_head);
        }
        other => panic!("expected PendingDecision, got {other:?}"),
    }
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_non_coder_matching_status_has_no_modified_warning() {
    let head = git_head().unwrap_or_default();
    let status = git_status().unwrap_or_default();
    let snap = test_snapshot(&head, &status);
    let result = verify_non_coder(&snap);
    assert!(matches!(result, VerifyResult::Ok { .. }));
    let warnings = warnings_of(&result);
    assert!(
        !warnings.iter().any(|w| w.contains("modified working tree")),
        "expected no modified-tree warning when status unchanged, got: {warnings:?}"
    );
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_non_coder_hard_error_when_dirty_baseline_changes() {
    let head = git_head().unwrap_or_default();
    let status = git_status().unwrap_or_default();
    let mut snap = test_snapshot(&head, &status);
    snap.working_tree_baseline = Some("__baseline_that_should_not_match__".to_string());

    let result = verify_non_coder(&snap);
    match result {
        VerifyResult::HardError { reason, .. } => {
            assert_eq!(reason, "reviewer_modified_working_tree");
        }
        other => panic!("expected HardError, got {other:?}"),
    }
}

fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = crate::state::test_fs_lock().lock();
    let prev = std::env::current_dir().unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::env::set_current_dir(dir).unwrap();
        f()
    }));
    std::env::set_current_dir(&prev).unwrap();
    outcome.unwrap()
}

fn git_available() -> bool {
    let _guard = crate::state::test_fs_lock().lock();
    std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_git(dir: &Path, args: &[&str]) {
    let _guard = crate::state::test_fs_lock().lock();
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git command should run");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

fn init_repo(dir: &Path) {
    run_git(dir, &["init", "-q"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test User"]);
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    run_git(dir, &["add", "README.md"]);
    run_git(dir, &["commit", "-q", "-m", "seed"]);
}

#[test]
#[serial_test::serial(process_cwd)]
fn git_status_dirty_reports_clean_then_dirty() {
    if !git_available() {
        return;
    }
    let temp = tempfile::TempDir::new().unwrap();
    init_repo(temp.path());
    with_cwd(temp.path(), || {
        assert!(!git_status_dirty(), "fresh repo must report clean");
        std::fs::write(temp.path().join("README.md"), "dirty\n").unwrap();
        assert!(git_status_dirty(), "modified README must report dirty");
    });
}

#[test]
#[serial_test::serial(process_cwd)]
fn capture_non_coder_writes_snapshot_with_head_and_status() {
    if !git_available() {
        return;
    }
    let repo = tempfile::TempDir::new().unwrap();
    init_repo(repo.path());
    let snapshot_dir = tempfile::TempDir::new().unwrap();
    with_cwd(repo.path(), || {
        capture_non_coder(snapshot_dir.path(), "auditor", GuardMode::AutoReset, false).unwrap();
    });
    let snap = read_snapshot(snapshot_dir.path()).expect("snapshot must exist");
    assert!(!snap.head.is_empty(), "head must be captured");
    assert_eq!(snap.mode, GuardMode::AutoReset);
    assert!(snap.working_tree_baseline.is_none());
}

#[test]
#[serial_test::serial(process_cwd)]
fn capture_coder_records_round_control_files() {
    if !git_available() {
        return;
    }
    let repo = tempfile::TempDir::new().unwrap();
    init_repo(repo.path());
    let session_dir = repo.path().join("session");
    let round_dir = session_dir.join("rounds").join("003");
    std::fs::create_dir_all(&round_dir).unwrap();
    std::fs::write(round_dir.join("task.toml"), "id = 1\n").unwrap();
    let snapshot_dir = tempfile::TempDir::new().unwrap();
    with_cwd(repo.path(), || {
        capture_coder(snapshot_dir.path(), &session_dir, 3).unwrap();
    });
    let snap = read_snapshot(snapshot_dir.path()).expect("snapshot must exist");
    assert!(
        snap.control_files.keys().any(|k| k.ends_with("task.toml")),
        "task.toml must be captured under control_files: {:?}",
        snap.control_files.keys().collect::<Vec<_>>()
    );
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_returns_ok_when_snapshot_file_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    // No snapshot file written here.
    let result = verify(dir.path(), "auditor");
    assert!(matches!(result, VerifyResult::Ok { warnings } if warnings.is_empty()));
}

#[test]
#[serial_test::serial(process_cwd)]
fn verify_dispatches_to_coder_path_when_stage_is_coder() {
    let snapshot_dir = tempfile::TempDir::new().unwrap();
    let mut control = BTreeMap::new();
    control.insert(
        snapshot_dir
            .path()
            .join("nonexistent.toml")
            .display()
            .to_string(),
        "expected".to_string(),
    );
    let snap = Snapshot {
        head: String::new(),
        git_status: String::new(),
        control_files: control,
        baseline_stash: None,
        mode: GuardMode::AutoReset,
        working_tree_baseline: None,
    };
    write_snapshot(snapshot_dir.path(), &snap).unwrap();
    // Dispatch via the public `verify` and expect the coder path's
    // forbidden_control_edit hard error (file is missing -> mismatched).
    let result = verify(snapshot_dir.path(), "coder");
    match result {
        VerifyResult::HardError { reason, .. } => {
            assert!(
                reason.starts_with("forbidden_control_edit"),
                "expected coder-path violation, got: {reason}"
            );
        }
        other => panic!("expected HardError from coder dispatch, got {other:?}"),
    }
}

#[test]
#[serial_test::serial(process_cwd)]
fn reset_hard_to_returns_false_when_head_does_not_resolve() {
    if !git_available() {
        return;
    }
    let temp = tempfile::TempDir::new().unwrap();
    init_repo(temp.path());
    with_cwd(temp.path(), || {
        // SHA that cannot exist in this fresh repo.
        assert!(!reset_hard_to("0000000000000000000000000000000000000000"));
    });
}
