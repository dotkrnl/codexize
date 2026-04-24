use codexize::state::{Message, MessageKind, RunRecord, RunStatus, SessionState, session_dir};

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
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = std::env::current_dir().expect("cwd");

    std::env::set_current_dir(temp.path()).expect("enter temp root");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::env::set_current_dir(cwd).expect("restore cwd");
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
                text: "drafting schema".to_string(),
            })
            .expect("append brief");
        state
            .append_message(&Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
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
