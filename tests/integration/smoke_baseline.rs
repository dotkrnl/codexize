use codexize::{
    smoke,
    state::{
        LaunchModes, Message, MessageKind, MessageSender, RunRecord, RunStatus, SessionState,
        session_dir,
    },
};
use std::{
    path::Path,
    sync::{Mutex, OnceLock},
};

fn sample_run(id: u64, status: RunStatus) -> RunRecord {
    RunRecord {
        id,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status,
        error: None,
        effort: codexize::adapters::EffortLevel::Normal,
        modes: LaunchModes::default(),
        hostname: Some("test-host".to_string()),
        mount_device_id: Some(42),
    }
}

fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
    static TEST_ROOT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = TEST_ROOT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::TempDir::new().expect("tempdir");
    let prev = std::env::var_os("CODEXIZE_ROOT");

    // SAFETY: env mutation is serialized by `TEST_ROOT_LOCK`.
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    result.expect("test panicked")
}

fn create_smoke_session() -> String {
    let session_id = format!(
        "smoke-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let mut state = SessionState::new(session_id.clone());
    state.modes.yolo = true;
    state.modes.cheap = true;
    state.agent_runs.push(sample_run(1, RunStatus::Done));
    state.agent_runs[0].modes = state.launch_modes();
    state.save().expect("save session");

    state
        .append_message(&Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: "claude-opus-4-7".to_string(),
                vendor: "anthropic".to_string(),
            },
            text: format!("root={}", std::env::var("CODEXIZE_ROOT").expect("root")),
        })
        .expect("append brief");
    state
        .append_message(&Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::End,
            sender: MessageSender::System,
            text: "done in 0m10s".to_string(),
        })
        .expect("append end");
    session_id
}

#[test]
fn smoke_baseline_matches_normalized_artifacts() {
    with_temp_root(|| {
        let session_id = create_smoke_session();
        let actual = smoke::normalize_session_artifacts(
            &session_dir(&session_id),
            &session_id,
            &std::env::var("CODEXIZE_ROOT").expect("root"),
        )
        .expect("normalize actual artifacts");
        let baseline =
            smoke::load_normalized_fixture_tree(Path::new("tests/fixtures/smoke_baseline"))
                .unwrap_or_else(|err| panic!("load baseline: {err}\n--- actual ---\n{actual:#?}"));
        let diff = smoke::diff_normalized_trees(&baseline, &actual);
        assert!(
            diff.is_empty(),
            "normalized smoke baseline drifted:\n{}",
            diff.join("\n")
        );
    });
}

#[test]
fn headless_gate_detects_when_live_smoke_is_unavailable() {
    if smoke::live_smoke_prereqs_available() {
        assert!(!smoke::headless_fallback_active());
    } else {
        // `cargo test` is already executing the existing integration suites in
        // this same invocation, so the fallback gate only needs to make the
        // skip explicit here instead of recursively spawning another test run.
        assert!(smoke::headless_fallback_active());
    }
}
