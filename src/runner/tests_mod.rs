use super::*;

#[test]
fn test_validate_toml_artifacts_all_valid() {
    let dir = tempfile::TempDir::new().unwrap();
    let p1 = dir.path().join("a.toml");
    let p2 = dir.path().join("b.toml");
    fs::write(&p1, "status = \"ok\"").unwrap();
    fs::write(&p2, "count = 42").unwrap();
    assert!(validate_toml_artifacts(&[p1.as_path(), p2.as_path()]).is_ok());
}

#[test]
fn test_validate_toml_artifacts_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("nope.toml");
    let result = validate_toml_artifacts(&[missing.as_path()]);
    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(msg.contains("missing"));
}

#[test]
fn test_validate_toml_artifacts_malformed() {
    let dir = tempfile::TempDir::new().unwrap();
    let bad = dir.path().join("bad.toml");
    fs::write(&bad, "not { valid } toml [").unwrap();
    let result = validate_toml_artifacts(&[bad.as_path()]);
    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(msg.contains("malformed TOML"));
}

#[test]
fn test_validate_toml_artifacts_non_toml_ignored() {
    let dir = tempfile::TempDir::new().unwrap();
    let md = dir.path().join("spec.md");
    fs::write(&md, "# Not TOML at all {{{{}}}}}").unwrap();
    assert!(validate_toml_artifacts(&[md.as_path()]).is_ok());
}

#[test]
fn test_validate_toml_artifacts_mix_missing_and_malformed() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("gone.toml");
    let bad = dir.path().join("bad.toml");
    fs::write(&bad, "[[[[broken").unwrap();
    let result = validate_toml_artifacts(&[missing.as_path(), bad.as_path()]);
    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(msg.contains("missing"));
    assert!(msg.contains("malformed"));
}

#[test]
fn test_extract_model_from_flag() {
    let cmd = vec![
        "claude".to_string(),
        "--model".to_string(),
        "opus-4".to_string(),
    ];
    assert_eq!(extract_model(&cmd), "opus-4");
}

#[test]
fn test_extract_model_from_equals() {
    let cmd = vec!["claude".to_string(), "--model=sonnet-4".to_string()];
    assert_eq!(extract_model(&cmd), "sonnet-4");
}

#[test]
fn test_extract_model_fallback_to_binary() {
    let cmd = vec!["/usr/bin/claude".to_string(), "--fast".to_string()];
    assert_eq!(extract_model(&cmd), "claude");
}

#[test]
fn finish_stamp_round_trip() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stamp.toml");
    let stamp = FinishStamp {
        finished_at: "2026-04-26T10:00:00Z".to_string(),
        exit_code: 0,
        head_before: "abc123".to_string(),
        head_after: "def456".to_string(),
        head_state: "stable".to_string(),
        signal_received: String::new(),
        working_tree_clean: true,
    };
    write_finish_stamp(&path, &stamp).unwrap();
    assert!(path.exists());
    let read = read_finish_stamp(&path).unwrap();
    assert_eq!(read, stamp);
}

#[test]
fn finish_stamp_atomic_write_no_partial_file_on_failure() {
    let dir = tempfile::TempDir::new().unwrap();
    // Use a read-only directory to force the write to fail.
    let ro_dir = dir.path().join("readonly");
    fs::create_dir(&ro_dir).unwrap();
    let mut perms = fs::metadata(&ro_dir).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&ro_dir, perms.clone()).unwrap();

    let path = ro_dir.join("stamp.toml");
    let stamp = FinishStamp {
        finished_at: "2026-04-26T10:00:00Z".to_string(),
        exit_code: 0,
        head_before: "abc123".to_string(),
        head_after: "def456".to_string(),
        head_state: "stable".to_string(),
        signal_received: String::new(),
        working_tree_clean: true,
    };
    let result = write_finish_stamp(&path, &stamp);
    assert!(result.is_err());

    // No partial file should remain.
    let entries: Vec<_> = fs::read_dir(&ro_dir).unwrap().flatten().collect();
    assert!(
        entries.is_empty(),
        "expected no partial files, got {:?}",
        entries
    );

    // Restore permissions so the temp dir can be cleaned up.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o700);
        let _ = fs::set_permissions(&ro_dir, perms);
    }
}

#[test]
fn finish_stamp_parses_required_fields() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stamp.toml");
    fs::write(
        &path,
        r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 1
head_before = "000000"
head_after = "111111"
head_state = "unstable"
"#,
    )
    .unwrap();
    let stamp = read_finish_stamp(&path).unwrap();
    assert_eq!(stamp.finished_at, "2026-04-26T10:00:00Z");
    assert_eq!(stamp.exit_code, 1);
    assert_eq!(stamp.head_before, "000000");
    assert_eq!(stamp.head_after, "111111");
    assert_eq!(stamp.head_state, "unstable");
    assert!(!stamp.working_tree_clean);
}

#[test]
fn finish_stamp_missing_field_is_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stamp.toml");
    fs::write(
        &path,
        r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 0
head_before = "abc"
head_after = "def"
"#,
    )
    .unwrap();
    assert!(read_finish_stamp(&path).is_err());
}

#[test]
fn finish_stamp_malformed_toml_is_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stamp.toml");
    fs::write(&path, "not { valid toml").unwrap();
    assert!(read_finish_stamp(&path).is_err());
}

#[test]
fn finish_stamp_ignores_unknown_fields() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stamp.toml");
    fs::write(
        &path,
        r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 0
head_before = "abc"
head_after = "def"
head_state = "stable"
extra_field = "ignored"
"#,
    )
    .unwrap();
    let stamp = read_finish_stamp(&path).unwrap();
    assert_eq!(stamp.head_state, "stable");
}

#[test]
fn finish_stamp_serialization_includes_working_tree_clean() {
    let stamp = FinishStamp {
        finished_at: "2026-04-26T10:00:00Z".to_string(),
        exit_code: 0,
        head_before: "abc123".to_string(),
        head_after: "def456".to_string(),
        head_state: "stable".to_string(),
        signal_received: String::new(),
        working_tree_clean: true,
    };

    let text = toml::to_string_pretty(&stamp).unwrap();
    assert!(text.contains("working_tree_clean = true"));
}

#[test]
fn shell_cmd_contains_stabilization_loop() {
    let cmd = build_shell_cmd(
        "claude -p prompt.md",
        "[Test]",
        "/tmp/status",
        "/tmp/status/run.txt",
        "/tmp/artifacts/run-finish",
        "/tmp/artifacts/run-finish/test-key.toml",
    );
    assert!(cmd.contains("git rev-parse HEAD"), "should capture HEAD");
    assert!(cmd.contains("head_state"), "should write head_state");
    assert!(
        cmd.contains(".git/index.lock"),
        "should wait for index.lock"
    );
    assert!(cmd.contains("stable"), "should mention stable state");
    assert!(cmd.contains("unstable"), "should mention unstable state");
    assert!(cmd.contains("mv "), "should atomically rename stamp");
    assert!(
        cmd.contains("CODEXIZE_STAMP_STABILIZE_MS"),
        "should read env budget"
    );
}

#[test]
fn interactive_shell_cmd_runs_agent_in_foreground() {
    let cmd = build_shell_cmd_with_mode(
        "codex -m gpt-5 prompt",
        "[Test]",
        "/tmp/status",
        "/tmp/status/run.txt",
        "/tmp/artifacts/run-finish",
        "/tmp/artifacts/run-finish/test-key.toml",
        ShellAgentMode::Foreground,
    );

    assert!(
        !cmd.contains("codex -m gpt-5 prompt &"),
        "interactive commands must keep terminal ownership by running in the foreground"
    );
    assert!(
        cmd.contains("codex -m gpt-5 prompt\nexit_code=$?"),
        "wrapper should capture the foreground agent exit status"
    );
}

#[test]
fn noninteractive_shell_cmd_keeps_background_child_for_signal_forwarding() {
    let cmd = build_shell_cmd(
        "codex exec prompt",
        "[Test]",
        "/tmp/status",
        "/tmp/status/run.txt",
        "/tmp/artifacts/run-finish",
        "/tmp/artifacts/run-finish/test-key.toml",
    );

    assert!(
        cmd.contains("codex exec prompt &\nchild_pid=$!\nwait \"$child_pid\""),
        "non-interactive commands should keep the signal-forwarding child wrapper"
    );
}

#[test]
fn shell_cmd_escapes_paths() {
    let cmd = build_shell_cmd(
        "echo hello",
        "[Test]",
        "/tmp/weird'path",
        "/tmp/weird'path/status.txt",
        "/tmp/weird'path/finish",
        "/tmp/weird'path/finish/key.toml",
    );
    // Escaped paths should contain the single-quote handling.
    assert!(
        cmd.contains("weird'\\''path"),
        "path should be shell-escaped"
    );
}

#[test]
fn shell_cmd_produces_stable_stamp_in_git_repo() {
    let dir = tempfile::TempDir::new().unwrap();
    let status_dir = dir.path().join("status");
    let status_path = status_dir.join("run.txt");
    let finish_dir = dir.path().join("run-finish");
    let stamp_path = finish_dir.join("test.toml");

    // Initialize a git repo with one commit.
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git init");
    fs::write(dir.path().join("file.txt"), "hello").unwrap();
    std::process::Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "test", "--no-gpg-sign"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git commit");
    fs::write(
        dir.path().join(".git").join("info").join("exclude"),
        "/status\n/run-finish\n",
    )
    .unwrap();

    let cmd = build_shell_cmd(
        "true",
        "[Test]",
        &status_dir.to_string_lossy(),
        &status_path.to_string_lossy(),
        &finish_dir.to_string_lossy(),
        &stamp_path.to_string_lossy(),
    );

    let output = std::process::Command::new("bash")
        .args(["-c", &cmd])
        .current_dir(dir.path())
        .output()
        .expect("bash");
    assert!(
        output.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(stamp_path.exists(), "stamp should exist");
    let stamp = read_finish_stamp(&stamp_path).unwrap();
    assert_eq!(stamp.exit_code, 0);
    assert_eq!(stamp.head_state, "stable");
    assert!(!stamp.head_before.is_empty());
    assert_eq!(stamp.head_before, stamp.head_after);
    assert!(stamp.working_tree_clean);

    // Status file should also contain the exit code.
    let status_text = fs::read_to_string(&status_path).unwrap();
    assert_eq!(status_text.trim(), "0");
}

#[test]
fn shell_cmd_produces_unstable_stamp_when_head_keeps_changing() {
    let dir = tempfile::TempDir::new().unwrap();
    let status_dir = dir.path().join("status");
    let status_path = status_dir.join("run.txt");
    let finish_dir = dir.path().join("run-finish");
    let stamp_path = finish_dir.join("test.toml");

    // Create a fake git that returns a different SHA each call.
    let bin_dir = dir.path().join("bin");
    fs::create_dir(&bin_dir).unwrap();
    let counter_file = dir.path().join("git_counter");
    let git_script = format!(
        r#"#!/bin/bash
if [ "$1" = "rev-parse" ] && [ "$2" = "HEAD" ]; then
    if [ -f "{counter}" ]; then
        c=$(cat "{counter}")
    else
        c=0
    fi
    c=$((c + 1))
    echo "$c" > "{counter}"
    printf '%040d\n' "$c"
    exit 0
fi
exit 1
"#,
        counter = counter_file.to_string_lossy()
    );
    fs::write(bin_dir.join("git"), git_script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(bin_dir.join("git")).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(bin_dir.join("git"), perms).unwrap();
    }

    // Create a fake .git directory so index.lock check works.
    fs::create_dir(dir.path().join(".git")).unwrap();

    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let mut new_path = std::ffi::OsString::from(&bin_dir);
    new_path.push(":");
    new_path.push(&path_env);

    let cmd = build_shell_cmd(
        "true",
        "[Test]",
        &status_dir.to_string_lossy(),
        &status_path.to_string_lossy(),
        &finish_dir.to_string_lossy(),
        &stamp_path.to_string_lossy(),
    );

    let output = std::process::Command::new("bash")
        .args(["-c", &cmd])
        .current_dir(dir.path())
        .env("PATH", &new_path)
        .output()
        .expect("bash");
    assert!(
        output.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(stamp_path.exists(), "stamp should exist");
    let stamp = read_finish_stamp(&stamp_path).unwrap();
    assert_eq!(stamp.exit_code, 0);
    assert_eq!(stamp.head_state, "unstable");
    assert!(!stamp.head_after.is_empty());
    assert!(!stamp.working_tree_clean);
}

#[test]
fn shell_cmd_marks_dirty_repo_in_finish_stamp() {
    let dir = tempfile::TempDir::new().unwrap();
    let status_dir = dir.path().join("status");
    let status_path = status_dir.join("run.txt");
    let finish_dir = dir.path().join("run-finish");
    let stamp_path = finish_dir.join("test.toml");

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git init");
    fs::write(dir.path().join("file.txt"), "hello").unwrap();
    std::process::Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "test", "--no-gpg-sign"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git commit");
    fs::write(
        dir.path().join(".git").join("info").join("exclude"),
        "/status\n/run-finish\n",
    )
    .unwrap();
    fs::write(dir.path().join("dirty.txt"), "untracked").unwrap();

    let cmd = build_shell_cmd(
        "true",
        "[Test]",
        &status_dir.to_string_lossy(),
        &status_path.to_string_lossy(),
        &finish_dir.to_string_lossy(),
        &stamp_path.to_string_lossy(),
    );

    let output = std::process::Command::new("bash")
        .args(["-c", &cmd])
        .current_dir(dir.path())
        .output()
        .expect("bash");
    assert!(
        output.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stamp = read_finish_stamp(&stamp_path).unwrap();
    assert!(!stamp.working_tree_clean);
}

#[test]
fn shell_cmd_writes_stamp_when_path_contains_spaces() {
    let dir = tempfile::Builder::new()
        .prefix("runner with spaces ")
        .tempdir()
        .unwrap();
    let status_dir = dir.path().join("status dir");
    let status_path = status_dir.join("run status.txt");
    let finish_dir = dir.path().join("run finish");
    let stamp_path = finish_dir.join("test key.toml");

    let cmd = build_shell_cmd(
        "true",
        "[Test]",
        &status_dir.to_string_lossy(),
        &status_path.to_string_lossy(),
        &finish_dir.to_string_lossy(),
        &stamp_path.to_string_lossy(),
    );

    let output = std::process::Command::new("bash")
        .args(["-c", &cmd])
        .current_dir(dir.path())
        .output()
        .expect("bash");
    assert!(
        output.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(stamp_path.exists(), "stamp should exist at escaped path");
    assert!(read_finish_stamp(&stamp_path).is_ok());
}

#[test]
fn shell_cmd_writes_stamp_when_interrupted() {
    let dir = tempfile::TempDir::new().unwrap();
    let status_dir = dir.path().join("status");
    let status_path = status_dir.join("run.txt");
    let finish_dir = dir.path().join("run-finish");
    let stamp_path = finish_dir.join("interrupted.toml");

    let cmd = build_shell_cmd(
        "sleep 30",
        "[Test]",
        &status_dir.to_string_lossy(),
        &status_path.to_string_lossy(),
        &finish_dir.to_string_lossy(),
        &stamp_path.to_string_lossy(),
    );

    let mut child = std::process::Command::new("bash")
        .args(["-c", &cmd])
        .current_dir(dir.path())
        .spawn()
        .expect("spawn bash");

    std::thread::sleep(Duration::from_millis(200));
    let pid = child.id().to_string();
    let kill_status = std::process::Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("kill");
    assert!(kill_status.success(), "failed to signal wrapper process");

    let mut attempts = 0;
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
            break;
        }
        attempts += 1;
        if attempts > 50 {
            let _ = child.kill();
            panic!("wrapper did not exit promptly after TERM");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(
        stamp_path.exists(),
        "interrupted run should still produce a finish stamp"
    );
    let stamp = read_finish_stamp(&stamp_path).expect("parse interrupted stamp");
    assert_ne!(
        stamp.exit_code, 0,
        "interrupted run should not report success"
    );
    assert_eq!(
        stamp.signal_received, "TERM",
        "interrupted run should record trapped signal"
    );
}

#[test]
fn shell_cmd_ignores_hup_without_forwarding_to_child() {
    let dir = tempfile::TempDir::new().unwrap();
    let status_dir = dir.path().join("status");
    let status_path = status_dir.join("run.txt");
    let finish_dir = dir.path().join("run-finish");
    let stamp_path = finish_dir.join("hup-ignored.toml");
    let child_done = dir.path().join("child-done");

    let agent_cmd = format!("sleep 1; touch {}", child_done.to_string_lossy());
    let cmd = build_shell_cmd(
        &agent_cmd,
        "[Test]",
        &status_dir.to_string_lossy(),
        &status_path.to_string_lossy(),
        &finish_dir.to_string_lossy(),
        &stamp_path.to_string_lossy(),
    );

    let mut child = std::process::Command::new("bash")
        .args(["-c", &cmd])
        .current_dir(dir.path())
        .spawn()
        .expect("spawn bash");

    std::thread::sleep(Duration::from_millis(200));
    let pid = child.id().to_string();
    let kill_status = std::process::Command::new("kill")
        .args(["-HUP", &pid])
        .status()
        .expect("send HUP");
    assert!(kill_status.success(), "failed to signal wrapper process");

    let mut attempts = 0;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            assert!(status.success(), "wrapper should ignore HUP: {status}");
            break;
        }
        attempts += 1;
        if attempts > 50 {
            let _ = child.kill();
            panic!("wrapper did not finish after ignored HUP");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(
        child_done.exists(),
        "child command should complete after wrapper receives HUP"
    );
    assert_eq!(
        fs::read_to_string(&status_path).expect("read status"),
        "0",
        "ignored HUP should not change exit status"
    );
    let stamp = read_finish_stamp(&stamp_path).expect("parse finish stamp");
    assert_eq!(stamp.exit_code, 0);
    assert_eq!(stamp.signal_received, "");
}

#[test]
fn finish_stamp_parses_old_stamp_without_signal_received() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stamp.toml");
    fs::write(
        &path,
        r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 1
head_before = "000000"
head_after = "111111"
head_state = "unstable"
"#,
    )
    .unwrap();
    let stamp = read_finish_stamp(&path).unwrap();
    assert_eq!(stamp.signal_received, "");
}

#[test]
fn run_returns_err_on_empty_command() {
    let result = run(
        "test-empty-cmd-session".to_string(),
        "audit".to_string(),
        "auditor".to_string(),
        vec![],
        vec![],
    );
    assert!(result.is_err(), "empty command must error, not panic");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("no command provided"),
        "unexpected error message: {msg}"
    );
}

fn with_temp_codexize_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::TempDir::new().unwrap();
    let prev = std::env::var_os("CODEXIZE_ROOT");
    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    outcome.unwrap()
}

#[test]
fn run_succeeds_on_zero_exit_with_no_required_artifacts() {
    with_temp_codexize_root(|| {
        // `true` is a POSIX no-op that exits 0 with no output.
        let result = run(
            "test-runner-true".to_string(),
            "audit".to_string(),
            "auditor".to_string(),
            vec![],
            vec!["true".to_string()],
        );
        assert!(result.is_ok(), "true should succeed: {:?}", result.err());
        // The runner writes a per-role log alongside the session dir.
        let log_path = state::session_dir("test-runner-true").join("auditor.log");
        assert!(log_path.exists(), "expected log at {log_path:?}");
    });
}

#[test]
fn run_returns_err_when_required_artifact_missing() {
    with_temp_codexize_root(|| {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("never-created.toml");
        let result = run(
            "test-runner-missing".to_string(),
            "audit".to_string(),
            "auditor".to_string(),
            vec![missing.to_string_lossy().into_owned()],
            vec!["true".to_string()],
        );
        let err = result.expect_err("missing artifact must error");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("required artifacts are missing"),
            "missing-artifact error context: {msg}"
        );
    });
}

#[test]
fn run_returns_err_when_command_exits_nonzero() {
    with_temp_codexize_root(|| {
        // `false` exits with status 1.
        let result = run(
            "test-runner-false".to_string(),
            "audit".to_string(),
            "auditor".to_string(),
            vec![],
            vec!["false".to_string()],
        );
        let err = result.expect_err("nonzero exit must error");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("agent command failed"),
            "exit-status error context: {msg}"
        );
    });
}

#[test]
fn run_child_with_timeout_returns_status_when_child_exits_quickly() {
    let launch = ChildLaunch::new("true")
        .stdin_null()
        .stdout_null()
        .stderr_null();
    let outcome = run_child_with_timeout(&launch, Duration::from_secs(2)).unwrap();
    let status = outcome.expect("child should exit before timeout");
    assert!(status.success(), "expected zero exit");
}

#[test]
fn run_child_with_timeout_returns_none_when_child_outruns_deadline() {
    let launch = ChildLaunch::new("sleep")
        .args(["10"])
        .stdin_null()
        .stdout_null()
        .stderr_null();
    let outcome = run_child_with_timeout(&launch, Duration::from_millis(150)).unwrap();
    assert!(
        outcome.is_none(),
        "expected timeout-killed result, got {outcome:?}"
    );
}

#[test]
fn run_child_with_timeout_propagates_spawn_failure() {
    let launch = ChildLaunch::new("/this/program/definitely/does/not/exist-xyz");
    let err = run_child_with_timeout(&launch, Duration::from_millis(100))
        .expect_err("spawning a missing binary must error before any timeout work");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("failed to spawn"),
        "spawn error context: {msg}"
    );
}

fn with_test_env<T>(
    repo_dir: &Path,
    vars: &[(&str, Option<String>)],
    f: impl FnOnce() -> T,
) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let previous_dir = std::env::current_dir().expect("cwd");
    let previous_vars = vars
        .iter()
        .map(|(key, _)| ((*key).to_string(), std::env::var_os(key)))
        .collect::<Vec<_>>();

    // SAFETY: serialized via test_fs_lock; restored before returning.
    unsafe {
        std::env::set_current_dir(repo_dir).expect("set current dir");
        for (key, value) in vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        std::env::set_current_dir(previous_dir).expect("restore current dir");
        for (key, value) in previous_vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
    shutdown_all_runs();
    outcome.unwrap()
}

fn init_git_repo(dir: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git init");
    fs::write(dir.join("tracked.txt"), "hello").expect("write tracked file");
    std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=codexize",
            "-c",
            "user.email=codexize@example.com",
            "commit",
            "-m",
            "test",
            "--no-gpg-sign",
        ])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git commit");
    fs::create_dir_all(dir.join(".git").join("info")).expect("git info dir");
    fs::write(dir.join(".git").join("info").join("exclude"), "/artifacts\n").expect("exclude");
}

fn write_test_acp_script(dir: &Path) -> PathBuf {
    let script = dir.join("artifacts").join("fake-acp.sh");
    fs::create_dir_all(script.parent().expect("script parent")).expect("script dir");
    fs::write(
        &script,
        r#"#!/bin/bash
set -euo pipefail

extract_id() {
    printf '%s\n' "$1" | sed -En 's/.*"id":([0-9]+).*/\1/p'
}

mode="${ACP_TEST_MODE:-success}"
artifact="${ACP_TEST_ARTIFACT:-}"
log_path="${ACP_TEST_LOG:-}"

while IFS= read -r line; do
    id="$(extract_id "$line")"
    case "$line" in
        *'"method":"initialize"'*)
            if [ -n "$log_path" ]; then
                printf '%s\n' "$$" >> "$log_path"
            fi
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"close":true}}}}\n' "$id"
            ;;
        *'"method":"session/new"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"sess-%s","configOptions":[]}}\n' "$id" "$$"
            ;;
        *'"method":"session/set_config_option"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"configOptions":[]}}\n' "$id"
            ;;
        *'"method":"session/prompt"'*)
            if [ "$mode" = "early_exit" ]; then
                exit 0
            fi
            if [ "$mode" = "sleep_forever" ]; then
                trap 'exit 0' TERM INT
                while true; do sleep 1; done
            fi
            if [ -n "$artifact" ] && [ "$mode" != "missing_artifact" ]; then
                mkdir -p "$(dirname "$artifact")"
                printf 'status = "ok"\n' > "$artifact"
            fi
            printf '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"done"}}}}\n'
            printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id"
            ;;
        *'"method":"session/close"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
            exit 0
            ;;
    esac
done
"#,
    )
    .expect("write fake ACP script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script).expect("script metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("script perms");
    }
    script
}

fn launch_test_run(dir: &Path) -> AgentRun {
    let prompt_path = dir.join("artifacts").join("prompt.md");
    fs::create_dir_all(prompt_path.parent().expect("prompt parent")).expect("prompt dir");
    fs::write(&prompt_path, "Implement the task").expect("write prompt");
    AgentRun {
        model: "model-x".to_string(),
        prompt_path,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
    }
}

fn wait_for_window_to_finish(window_name: &str) {
    for _ in 0..200 {
        if !window_is_active(window_name) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("managed ACP window did not finish: {window_name}");
}

#[test]
fn launch_interactive_bails_when_acp_cli_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("status.txt");
    let artifacts_dir = dir.path().join("artifacts");
    with_test_env(
        dir.path(),
        &[(
            "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
            Some("/definitely/missing/codex-acp".to_string()),
        )],
        || {
            let result = launch_interactive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                false,
                &status_path,
                "run-1",
                &artifacts_dir,
                None,
            );
            let err = result.expect_err("missing CLI must bail before launch");
            let msg = format!("{:#}", err);
            assert!(msg.contains("agent CLI not found"), "unexpected error: {msg}");
        },
    );
}

#[test]
fn launch_noninteractive_bails_when_acp_cli_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("status.txt");
    let artifacts_dir = dir.path().join("artifacts");
    with_test_env(
        dir.path(),
        &[(
            "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
            Some("/definitely/missing/codex-acp".to_string()),
        )],
        || {
            let result = launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                &status_path,
                "run-2",
                &artifacts_dir,
                None,
            );
            let err = result.expect_err("missing CLI must bail before launch");
            let msg = format!("{:#}", err);
            assert!(msg.contains("agent CLI not found"), "unexpected error: {msg}");
        },
    );
}

#[test]
fn acp_launch_writes_finish_stamp_and_status_on_success() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("run-status").join("coder.txt");
    let artifacts_dir = dir.path().join("artifacts");
    let artifact_path = artifacts_dir.join("coder_summary.toml");
    let stamp_path = artifacts_dir.join("run-finish").join("coder-run.toml");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
            (
                "ACP_TEST_ARTIFACT",
                Some(artifact_path.display().to_string()),
            ),
        ],
        || {
            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                &status_path,
                "coder-run",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("launch ACP run");

            wait_for_window_to_finish("[Coder]");

            assert_eq!(fs::read_to_string(&status_path).expect("read status").trim(), "0");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 0);
            assert_eq!(stamp.head_state, "stable");
            assert!(stamp.working_tree_clean);
            assert!(artifact_path.exists(), "expected validated artifact");
        },
    );
}

#[test]
fn acp_launch_fails_when_required_artifact_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("run-status").join("coder.txt");
    let artifacts_dir = dir.path().join("artifacts");
    let artifact_path = artifacts_dir.join("coder_summary.toml");
    let stamp_path = artifacts_dir.join("run-finish").join("coder-run.toml");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("missing_artifact".to_string())),
        ],
        || {
            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                &status_path,
                "coder-run",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("launch ACP run");

            wait_for_window_to_finish("[Coder]");

            assert_eq!(fs::read_to_string(&status_path).expect("read status").trim(), "1");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 1);
            assert!(!artifact_path.exists(), "artifact should be absent");
        },
    );
}

#[test]
fn acp_launch_marks_early_process_exit_as_failed() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("run-status").join("coder.txt");
    let artifacts_dir = dir.path().join("artifacts");
    let stamp_path = artifacts_dir.join("run-finish").join("coder-run.toml");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("early_exit".to_string())),
        ],
        || {
            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                &status_path,
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            wait_for_window_to_finish("[Coder]");

            assert_eq!(fs::read_to_string(&status_path).expect("read status").trim(), "1");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 1);
        },
    );
}

#[test]
fn acp_launch_enforces_single_active_run() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("run-status").join("coder.txt");
    let artifacts_dir = dir.path().join("artifacts");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("sleep_forever".to_string())),
        ],
        || {
            launch_noninteractive(
                "[Coder 1]",
                &run,
                VendorKind::Codex,
                &status_path,
                "coder-one",
                &artifacts_dir,
                None,
            )
            .expect("first launch");

            let err = launch_noninteractive(
                "[Coder 2]",
                &run,
                VendorKind::Codex,
                &status_path,
                "coder-two",
                &artifacts_dir,
                None,
            )
            .expect_err("second active run must be rejected");
            let msg = format!("{:#}", err);
            assert!(msg.contains("one active ACP run"), "unexpected error: {msg}");

            cancel_windows_matching("[Coder 1]");
            wait_for_window_to_finish("[Coder 1]");
        },
    );
}

#[test]
fn acp_launch_cleans_up_child_on_cancel() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("run-status").join("coder.txt");
    let artifacts_dir = dir.path().join("artifacts");
    let stamp_path = artifacts_dir.join("run-finish").join("coder-run.toml");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("sleep_forever".to_string())),
        ],
        || {
            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                &status_path,
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            cancel_windows_matching("[Coder]");
            wait_for_window_to_finish("[Coder]");

            assert_eq!(fs::read_to_string(&status_path).expect("read status").trim(), "143");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 143);
            assert_eq!(stamp.signal_received, "TERM");
        },
    );
}

#[test]
fn acp_launch_starts_fresh_process_for_each_stage() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let status_path = dir.path().join("artifacts").join("run-status").join("stage.txt");
    let artifacts_dir = dir.path().join("artifacts");
    let artifact_path = artifacts_dir.join("stage.toml");
    let log_path = dir.path().join("agent-pids.log");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
            (
                "ACP_TEST_ARTIFACT",
                Some(artifact_path.display().to_string()),
            ),
            ("ACP_TEST_LOG", Some(log_path.display().to_string())),
        ],
        || {
            launch_noninteractive(
                "[Stage 1]",
                &run,
                VendorKind::Codex,
                &status_path,
                "stage-one",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("first launch");
            wait_for_window_to_finish("[Stage 1]");

            launch_noninteractive(
                "[Stage 2]",
                &run,
                VendorKind::Codex,
                &status_path,
                "stage-two",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("second launch");
            wait_for_window_to_finish("[Stage 2]");

            let pids = fs::read_to_string(&log_path)
                .expect("read pid log")
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert_eq!(pids.len(), 2, "expected one initialize per launch");
            assert_ne!(pids[0], pids[1], "expected fresh ACP process per stage");
        },
    );
}
