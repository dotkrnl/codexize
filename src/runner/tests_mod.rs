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
fn acp_text_stream_updates_partial_message_and_splits_paragraphs() {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::TempDir::new().unwrap();
    let prev = std::env::var_os("CODEXIZE_ROOT");
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }

    let session_id = "runner-live-stream";
    let mut state = SessionState::new(session_id.to_string());
    state.agent_runs.push(crate::state::RunRecord {
        id: 7,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "model".to_string(),
        vendor: "vendor".to_string(),
        window_name: "[Live]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.save().unwrap();
    let launch = ManagedAcpLaunch {
        resolved: crate::acp::AcpResolvedLaunch {
            vendor: VendorKind::Codex,
            interactive: true,
            spawn: crate::acp::AcpSpawnSpec {
                program: "true".to_string(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
            },
            session: crate::acp::AcpSessionSpec {
                cwd: std::env::current_dir().unwrap(),
                prompt: PromptPayload::Text("prompt".to_string()),
                model: "model".to_string(),
                requested_effort: crate::adapters::EffortLevel::Normal,
                effective_effort: crate::adapters::EffortLevel::Normal,
                reasoning_effort: crate::acp::AcpReasoningEffort::Medium,
                permission_mode: crate::acp::AcpPermissionMode::Ask,
                interactive: true,
                modes: crate::state::LaunchModes::default(),
                required_artifacts: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
            },
        },
        window_name: "[Live]".to_string(),
        session_id: Some(session_id.to_string()),
        stamp_path: temp.path().join("stamp.toml"),
        cause_path: temp.path().join("cause.txt"),
        required_artifact: None,
    };
    let mut stream = AcpTextStream::new();

    stream.push_text(&launch, "thinking", MessageKind::AgentThought);
    stream.push_text(&launch, " aloud", MessageKind::AgentThought);
    let messages = SessionState::load_messages(session_id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "thinking aloud");

    stream.push_text(&launch, "\n\nnext", MessageKind::AgentThought);
    let messages = SessionState::load_messages(session_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "thinking aloud");
    assert_eq!(messages[1].text, "next");

    unsafe {
        match prev {
            Some(value) => std::env::set_var("CODEXIZE_ROOT", value),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
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

fn with_test_env<T>(repo_dir: &Path, vars: &[(&str, Option<String>)], f: impl FnOnce() -> T) -> T {
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
    fs::write(
        dir.join(".git").join("info").join("exclude"),
        "/artifacts\n",
    )
    .expect("exclude");
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
prompt_done_path="${ACP_TEST_PROMPT_DONE:-}"
prompt_log_path="${ACP_TEST_PROMPT_LOG:-}"
thought_text="${ACP_TEST_THOUGHT:-}"
thought_chunks="${ACP_TEST_THOUGHT_CHUNKS:-}"

while IFS= read -r line; do
    id="$(extract_id "$line")"
    case "$line" in
        *'"method":"initialize"'*)
            if [ -n "$log_path" ]; then
                printf '%s\n' "$$" >> "$log_path"
            fi
            if [ "$mode" = "invalid_initialize_json" ]; then
                printf '{"jsonrpc":\n'
                exit 0
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
            if [ -n "$prompt_log_path" ]; then
                printf '%s\n' "$line" >> "$prompt_log_path"
            fi
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
            if [ -n "$thought_text" ]; then
                printf '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"text":"%s"}}}}\n' "$thought_text"
            fi
            if [ -n "$thought_chunks" ]; then
                old_ifs="$IFS"
                IFS='|'
                for chunk in $thought_chunks; do
                    printf '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"text":"%s"}}}}\n' "$chunk"
                done
                IFS="$old_ifs"
            fi
            printf '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"done"}}}}\n'
            printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id"
            if [ -n "$prompt_done_path" ]; then
                printf 'done\n' > "$prompt_done_path"
            fi
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
        let mut perms = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
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

fn wait_for_run_label_to_finish(window_name: &str) {
    for _ in 0..200 {
        if !run_label_is_active(window_name) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("managed ACP run label did not finish: {window_name}");
}

fn wait_until_run_label_active(window_name: &str) {
    for _ in 0..200 {
        if run_label_is_active(window_name) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("managed ACP run label did not become active: {window_name}");
}

fn wait_until_run_label_waiting_for_input(window_name: &str) {
    for _ in 0..200 {
        if run_label_is_waiting_for_input(window_name) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("managed ACP run label did not wait for input: {window_name}");
}

fn wait_for_path(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("path did not appear: {}", path.display());
}

fn wait_for_file_to_contain(path: &Path, needle: &str) {
    for _ in 0..200 {
        if fs::read_to_string(path)
            .map(|text| text.contains(needle))
            .unwrap_or(false)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{} did not contain {needle:?}", path.display());
}

#[test]
fn launch_interactive_bails_when_acp_cli_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let run = launch_test_run(dir.path());

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
                "run-1",
                &artifacts_dir,
                None,
            );
            let err = result.expect_err("missing CLI must bail before launch");
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("agent CLI not found"),
                "unexpected error: {msg}"
            );
        },
    );
}

#[test]
fn launch_noninteractive_bails_when_acp_cli_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let run = launch_test_run(dir.path());

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
                "run-2",
                &artifacts_dir,
                None,
            );
            let err = result.expect_err("missing CLI must bail before launch");
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("agent CLI not found"),
                "unexpected error: {msg}"
            );
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_writes_finish_stamp_on_success() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
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
                "coder-run",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 0);
            assert_eq!(stamp.head_state, "stable");
            assert!(stamp.working_tree_clean);
            assert!(artifact_path.exists(), "expected validated artifact");
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn interactive_acp_end_turn_keeps_run_alive_until_local_exit() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let mut run = launch_test_run(dir.path());
    run.modes.interactive = true;
    let artifacts_dir = dir.path().join("artifacts");
    let stamp_path = artifacts_dir
        .join("run-finish")
        .join("interactive-run.toml");
    let prompt_done_path = artifacts_dir.join("prompt-done.txt");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
            (
                "ACP_TEST_PROMPT_DONE",
                Some(prompt_done_path.display().to_string()),
            ),
            ("CODEXIZE_STAMP_STABILIZE_MS", Some("100".to_string())),
            (
                "CODEXIZE_STAMP_STABILIZE_INTERVAL_MS",
                Some("10".to_string()),
            ),
        ],
        || {
            launch_interactive(
                "[Brainstorm]",
                &run,
                VendorKind::Codex,
                "interactive-run",
                &artifacts_dir,
                None,
            )
            .expect("launch interactive ACP run");

            wait_until_run_label_active("[Brainstorm]");
            wait_for_path(&prompt_done_path);
            std::thread::sleep(Duration::from_millis(300));

            assert!(
                run_label_is_active("[Brainstorm]"),
                "interactive run must stay active after ACP end_turn"
            );
            assert!(
                !stamp_path.exists(),
                "interactive run must not write a finish stamp before local /exit"
            );

            request_run_label_exit("[Brainstorm]");
            wait_for_run_label_to_finish("[Brainstorm]");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp after exit");
            assert_eq!(stamp.exit_code, 143);
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn interactive_acp_input_is_sent_as_followup_prompt() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let mut run = launch_test_run(dir.path());
    run.modes.interactive = true;
    let artifacts_dir = dir.path().join("artifacts");
    let prompt_done_path = artifacts_dir.join("prompt-done.txt");
    let prompt_log_path = artifacts_dir.join("prompt-log.jsonl");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
            (
                "ACP_TEST_PROMPT_DONE",
                Some(prompt_done_path.display().to_string()),
            ),
            (
                "ACP_TEST_PROMPT_LOG",
                Some(prompt_log_path.display().to_string()),
            ),
            ("CODEXIZE_STAMP_STABILIZE_MS", Some("100".to_string())),
            (
                "CODEXIZE_STAMP_STABILIZE_INTERVAL_MS",
                Some("10".to_string()),
            ),
        ],
        || {
            launch_interactive(
                "[Brainstorm]",
                &run,
                VendorKind::Codex,
                "interactive-input-run",
                &artifacts_dir,
                None,
            )
            .expect("launch interactive ACP run");

            wait_until_run_label_active("[Brainstorm]");
            wait_for_path(&prompt_done_path);
            wait_until_run_label_waiting_for_input("[Brainstorm]");

            assert!(
                !send_run_label_input("[Brainstorm]", "   ".to_string()),
                "blank input must not advance an interactive turn"
            );
            assert!(send_run_label_input(
                "[Brainstorm]",
                "hello from operator".to_string()
            ));
            wait_for_file_to_contain(&prompt_log_path, "hello from operator");

            request_run_label_exit("[Brainstorm]");
            wait_for_run_label_to_finish("[Brainstorm]");
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn interactive_acp_input_is_rejected_until_prompt_turn_finishes() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let mut run = launch_test_run(dir.path());
    run.modes.interactive = true;
    let artifacts_dir = dir.path().join("artifacts");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("sleep_forever".to_string())),
            ("CODEXIZE_STAMP_STABILIZE_MS", Some("100".to_string())),
            (
                "CODEXIZE_STAMP_STABILIZE_INTERVAL_MS",
                Some("10".to_string()),
            ),
        ],
        || {
            launch_interactive(
                "[Brainstorm]",
                &run,
                VendorKind::Codex,
                "interactive-not-ready-run",
                &artifacts_dir,
                None,
            )
            .expect("launch interactive ACP run");

            wait_until_run_label_active("[Brainstorm]");
            assert!(
                !run_label_is_waiting_for_input("[Brainstorm]"),
                "run should not be waiting while the initial prompt is still in flight"
            );
            assert!(
                !send_run_label_input("[Brainstorm]", "too early".to_string()),
                "input must not be queued before the agent asks for it"
            );

            request_run_label_exit("[Brainstorm]");
            wait_for_run_label_to_finish("[Brainstorm]");
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_persists_agent_message_chunks_as_agent_text() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let session_id = "runner-agent-text";
    let session_root = dir.path().join(".codexize");
    let artifacts_dir = session_root
        .join("sessions")
        .join(session_id)
        .join("artifacts");
    let mut state = crate::state::SessionState::new(session_id.to_string());
    let run_id = state.create_run_record(
        "coder".to_string(),
        Some(4),
        5,
        1,
        "model-x".to_string(),
        "codex".to_string(),
        "[Coder]".to_string(),
        crate::adapters::EffortLevel::Normal,
        crate::state::LaunchModes::default(),
    );
    with_test_env(
        dir.path(),
        &[
            ("CODEXIZE_ROOT", Some(session_root.display().to_string())),
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
        ],
        || {
            state.save().expect("save session");

            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");

            let messages =
                crate::state::SessionState::load_messages(session_id).expect("load messages");
            assert!(
                messages.iter().any(|message| {
                    message.run_id == run_id
                        && message.kind == crate::state::MessageKind::AgentText
                        && matches!(message.sender, crate::state::MessageSender::Agent { .. })
                        && message.text == "done"
                }),
                "expected persisted AgentText message, got {messages:?}"
            );
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_persists_thought_chunks_as_agent_thought() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let session_id = "runner-agent-thought";
    let session_root = dir.path().join(".codexize");
    let artifacts_dir = session_root
        .join("sessions")
        .join(session_id)
        .join("artifacts");
    let mut state = crate::state::SessionState::new(session_id.to_string());
    let run_id = state.create_run_record(
        "coder".to_string(),
        Some(4),
        5,
        1,
        "model-x".to_string(),
        "codex".to_string(),
        "[Coder]".to_string(),
        crate::adapters::EffortLevel::Normal,
        crate::state::LaunchModes::default(),
    );
    with_test_env(
        dir.path(),
        &[
            ("CODEXIZE_ROOT", Some(session_root.display().to_string())),
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
            ("ACP_TEST_THOUGHT", Some("private reasoning".to_string())),
        ],
        || {
            state.save().expect("save session");

            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");

            let messages =
                crate::state::SessionState::load_messages(session_id).expect("load messages");
            assert!(messages.iter().any(|message| {
                message.run_id == run_id
                    && message.kind == crate::state::MessageKind::AgentThought
                    && message.text == "private reasoning"
            }));
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_concatenates_thought_chunks_per_turn() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let session_id = "runner-agent-thought-concat";
    let session_root = dir.path().join(".codexize");
    let artifacts_dir = session_root
        .join("sessions")
        .join(session_id)
        .join("artifacts");
    let mut state = crate::state::SessionState::new(session_id.to_string());
    let run_id = state.create_run_record(
        "coder".to_string(),
        Some(4),
        5,
        1,
        "model-x".to_string(),
        "codex".to_string(),
        "[Coder]".to_string(),
        crate::adapters::EffortLevel::Normal,
        crate::state::LaunchModes::default(),
    );
    with_test_env(
        dir.path(),
        &[
            ("CODEXIZE_ROOT", Some(session_root.display().to_string())),
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("success".to_string())),
            (
                "ACP_TEST_THOUGHT_CHUNKS",
                Some("Let| me| inspect| this".to_string()),
            ),
        ],
        || {
            state.save().expect("save session");

            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");

            let thoughts = crate::state::SessionState::load_messages(session_id)
                .expect("load messages")
                .into_iter()
                .filter(|message| {
                    message.run_id == run_id
                        && message.kind == crate::state::MessageKind::AgentThought
                })
                .map(|message| message.text)
                .collect::<Vec<_>>();
            assert_eq!(thoughts, vec!["Let me inspect this".to_string()]);
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_fails_when_required_artifact_is_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
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
                "coder-run",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 1);
            assert!(!artifact_path.exists(), "artifact should be absent");
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_marks_early_process_exit_as_failed() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
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
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 1);
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_records_cause_when_transport_init_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
    let artifacts_dir = dir.path().join("artifacts");
    let stamp_path = artifacts_dir.join("run-finish").join("coder-run.toml");
    let cause_path = artifacts_dir.join("run-finish").join("coder-run.cause.txt");
    with_test_env(
        dir.path(),
        &[
            (
                "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                Some(script.display().to_string()),
            ),
            ("ACP_TEST_MODE", Some("invalid_initialize_json".to_string())),
        ],
        || {
            launch_noninteractive(
                "[Coder]",
                &run,
                VendorKind::Codex,
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            wait_for_run_label_to_finish("[Coder]");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 1);
            let cause = fs::read_to_string(&cause_path).expect("read launch cause");
            assert!(
                cause.contains("invalid ACP JSON message"),
                "unexpected cause text: {cause}"
            );
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_enforces_single_active_run() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
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
                "coder-one",
                &artifacts_dir,
                None,
            )
            .expect("first launch");

            let err = launch_noninteractive(
                "[Coder 2]",
                &run,
                VendorKind::Codex,
                "coder-two",
                &artifacts_dir,
                None,
            )
            .expect_err("second active run must be rejected");
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("one active ACP run"),
                "unexpected error: {msg}"
            );

            cancel_run_labels_matching("[Coder 1]");
            wait_for_run_label_to_finish("[Coder 1]");
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_cleans_up_child_on_cancel() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
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
                "coder-run",
                &artifacts_dir,
                None,
            )
            .expect("launch ACP run");

            cancel_run_labels_matching("[Coder]");
            wait_for_run_label_to_finish("[Coder]");
            let stamp = read_finish_stamp(&stamp_path).expect("read finish stamp");
            assert_eq!(stamp.exit_code, 143);
            assert_eq!(stamp.signal_received, "TERM");
        },
    );
}

#[test]
#[ignore = "managed ACP subprocess integration; run explicitly with --ignored"]
fn acp_launch_starts_fresh_process_for_each_stage() {
    let dir = tempfile::TempDir::new().unwrap();
    init_git_repo(dir.path());
    let script = write_test_acp_script(dir.path());
    let run = launch_test_run(dir.path());
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
                "stage-one",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("first launch");
            wait_for_run_label_to_finish("[Stage 1]");

            launch_noninteractive(
                "[Stage 2]",
                &run,
                VendorKind::Codex,
                "stage-two",
                &artifacts_dir,
                Some(&artifact_path),
            )
            .expect("second launch");
            wait_for_run_label_to_finish("[Stage 2]");

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
