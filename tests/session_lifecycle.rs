use codexize::state::{
    Message, MessageKind, MessageSender, RunRecord, RunStatus, SessionState, session_dir,
};
use std::sync::{Mutex, OnceLock};

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

#[test]
fn session_round_trips_schema_v2_runs_and_messages() {
    with_temp_root(|| {
        let mut state = SessionState::new("integration-session".to_string());
        state.agent_runs.push(sample_run(1, RunStatus::Done));
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
                text: "drafting schema".to_string(),
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

        let loaded_state = SessionState::load("integration-session").expect("load session");
        let loaded_messages =
            SessionState::load_messages("integration-session").expect("load messages");

        assert_eq!(loaded_state.schema_version, 2);
        assert_eq!(loaded_state.agent_runs.len(), 1);
        assert_eq!(loaded_state.agent_runs[0].status, RunStatus::Done);
        assert_eq!(loaded_messages.len(), 2);
        assert_eq!(loaded_messages[0].kind, MessageKind::Brief);
        assert_eq!(loaded_messages[1].kind, MessageKind::End);
    });
}

#[test]
fn session_round_trips_failed_unverified_runs() {
    with_temp_root(|| {
        let mut state = SessionState::new("integration-unverified".to_string());
        state
            .agent_runs
            .push(sample_run(9, RunStatus::FailedUnverified));
        state.agent_runs[0].error = Some(
            "failed_unverified: missing finish stamp at /tmp/run-finish/coder-t1-r1-a1.toml"
                .to_string(),
        );
        state.save().expect("save session");

        let loaded_state = SessionState::load("integration-unverified").expect("load session");

        assert_eq!(loaded_state.agent_runs.len(), 1);
        assert_eq!(
            loaded_state.agent_runs[0].status,
            RunStatus::FailedUnverified
        );
        assert!(
            loaded_state.agent_runs[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("run-finish")
        );
    });
}

#[test]
fn resuming_missing_window_writes_end_message_and_persists_failure() {
    with_temp_root(|| {
        let mut state = SessionState::new("resume-missing-window".to_string());
        state.agent_runs.push(sample_run(7, RunStatus::Running));
        state.save().expect("save session");

        let resumed = state.resume_running_runs(&[]).expect("resume");
        assert_eq!(resumed, None);

        let reloaded = SessionState::load("resume-missing-window").expect("reload session");
        let messages =
            SessionState::load_messages("resume-missing-window").expect("reload messages");

        assert_eq!(reloaded.agent_runs[0].status, RunStatus::Failed);
        assert_eq!(
            reloaded.agent_runs[0].error.as_deref(),
            Some("window missing on resume")
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, MessageKind::End);
        assert!(messages[0].text.contains("window missing on resume"));

        let session_path = session_dir("resume-missing-window").join("session.toml");
        assert!(session_path.exists());
    });
}
