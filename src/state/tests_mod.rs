use super::*;

#[test]
fn pending_guard_decision_defaults_to_none_when_absent() {
    let toml_text = r#"
session_id = "abc"
schema_version = 2
current_phase = "IdeaInput"
"#;
    let state: SessionState =
        toml::from_str(toml_text).expect("legacy v2 session state must deserialize");
    assert!(state.pending_guard_decision.is_none());
}

#[test]
fn session_modes_default_to_off_when_absent() {
    let toml_text = r#"
session_id = "abc"
schema_version = 2
current_phase = "IdeaInput"
"#;
    let state: SessionState =
        toml::from_str(toml_text).expect("legacy v2 session state must deserialize");
    assert_eq!(state.modes, Modes::default());
    assert_eq!(state.launch_modes(), LaunchModes::default());
}

#[test]
fn session_modes_round_trip() {
    let mut state = SessionState::new("s".to_string());
    state.modes.yolo = true;
    state.modes.cheap = true;

    let text = toml::to_string(&state).expect("serialize");
    assert!(text.contains("[modes]"));
    let back: SessionState = toml::from_str(&text).expect("deserialize");

    assert_eq!(back.modes, state.modes);
    assert_eq!(
        back.launch_modes(),
        LaunchModes {
            yolo: true,
            cheap: true,
            interactive: false,
        }
    );
}

#[test]
fn effort_for_uses_tough_only_for_yolo_idea_and_planning() {
    let modes = LaunchModes {
        yolo: true,
        cheap: false,
        interactive: false,
    };

    assert_eq!(
        modes.effort_for(EffortLevel::Normal, crate::selection::SelectionPhase::Idea),
        EffortLevel::Tough
    );
    assert_eq!(
        modes.effort_for(EffortLevel::Low, crate::selection::SelectionPhase::Planning),
        EffortLevel::Tough
    );
}

#[test]
fn effort_for_preserves_requested_effort_for_build_and_review_under_yolo() {
    let modes = LaunchModes {
        yolo: true,
        cheap: false,
        interactive: false,
    };

    for requested in [EffortLevel::Low, EffortLevel::Normal, EffortLevel::Tough] {
        assert_eq!(
            modes.effort_for(requested, crate::selection::SelectionPhase::Build),
            requested
        );
        assert_eq!(
            modes.effort_for(requested, crate::selection::SelectionPhase::Review),
            requested
        );
    }
}

#[test]
fn effort_for_cheap_mode_wins_over_yolo_for_all_phases() {
    let modes = LaunchModes {
        yolo: true,
        cheap: true,
        interactive: false,
    };

    for phase in crate::selection::SelectionPhase::ALL {
        assert_eq!(
            modes.effort_for(EffortLevel::Tough, phase),
            EffortLevel::Low
        );
        assert_eq!(
            modes.effort_for(EffortLevel::Normal, phase),
            EffortLevel::Low
        );
    }
}

#[test]
fn pending_guard_decision_round_trips() {
    let mut state = SessionState::new("s".to_string());
    state.pending_guard_decision = Some(PendingGuardDecision {
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 2,
        run_id: 7,
        captured_head: "abc".to_string(),
        current_head: "def".to_string(),
        warnings: vec!["w".to_string()],
    });
    let text = toml::to_string(&state).expect("serialize");
    let back: SessionState = toml::from_str(&text).expect("deserialize");
    assert_eq!(back.pending_guard_decision, state.pending_guard_decision);
}

fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = test_fs_lock().lock().unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::TempDir::new().unwrap();
    let prev = std::env::var_os("CODEXIZE_ROOT");

    // SAFETY: `set_var`/`remove_var` are not thread-safe on *nix; the
    // `test_fs_lock` mutex serializes every test that touches the env.
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
    result.unwrap()
}

#[test]
fn test_run_record_lifecycle_create_to_done() {
    let mut runs = Vec::new();
    let run = RunRecord {
        id: 1,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    };
    runs.push(run);

    assert_eq!(runs[0].status, RunStatus::Running);
    assert!(runs[0].ended_at.is_none());
}

#[test]
fn test_run_record_transition_to_done() {
    let mut run = RunRecord {
        id: 1,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    };

    run.status = RunStatus::Done;
    run.ended_at = Some(chrono::Utc::now());

    assert_eq!(run.status, RunStatus::Done);
    assert!(run.ended_at.is_some());
    assert!(run.error.is_none());
}

#[test]
fn test_run_record_transition_to_failed() {
    let mut run = RunRecord {
        id: 1,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    };

    run.status = RunStatus::Failed;
    run.ended_at = Some(chrono::Utc::now());
    run.error = Some("validation failed".to_string());

    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.ended_at.is_some());
    assert_eq!(run.error.as_deref(), Some("validation failed"));
}

#[test]
fn test_message_creation() {
    let msg = Message {
        ts: chrono::Utc::now(),
        run_id: 1,
        kind: MessageKind::Brief,
        sender: MessageSender::Agent {
            model: "gpt-5".to_string(),
            vendor: "openai".to_string(),
        },
        text: "Exploring codebase".to_string(),
    };

    assert_eq!(msg.run_id, 1);
    assert_eq!(msg.kind, MessageKind::Brief);
    assert_eq!(msg.text, "Exploring codebase");
}

#[test]
fn test_message_kind_started_deserializes() {
    let kind = serde_json::from_str::<MessageKind>("\"Started\"");
    assert!(kind.is_ok(), "Started message kind must deserialize");
}

#[test]
fn test_node_creation() {
    let node = Node {
        label: "Brainstorm".to_string(),
        kind: NodeKind::Stage,
        status: NodeStatus::Done,
        summary: "completed".to_string(),
        children: vec![],
        run_id: None,
        leaf_run_id: Some(1),
    };

    assert_eq!(node.label, "Brainstorm");
    assert_eq!(node.kind, NodeKind::Stage);
    assert_eq!(node.leaf_run_id, Some(1));
}

#[test]
fn test_session_state_schema_v3() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-session".to_string());
        state.schema_version = 3;
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        state.save().unwrap();
        let loaded = SessionState::load("test-session").unwrap();

        assert_eq!(loaded.schema_version, 3);
        assert_eq!(loaded.agent_runs.len(), 1);
        assert_eq!(loaded.agent_runs[0].id, 1);
    });
}

#[test]
fn test_session_state_v2_rejected_after_v3_bump() {
    with_temp_root(|| {
        // A well-formed v2 file must now be rejected — the schema is hard-versioned
        // and there is no v2->v3 migration.
        let dir = session_dir("test-v2-rejected");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        std::fs::write(
            &path,
            r#"
session_id = "test-v2-rejected"
schema_version = 2
current_phase = "IdeaInput"
"#,
        )
        .unwrap();

        let result = SessionState::load("test-v2-rejected");
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("schema v2") || err_msg.contains("archive"),
            "v2 rejection message must mention version or archive: {err_msg}"
        );
    });
}

#[test]
fn test_new_session_defaults_to_v3_with_zero_validation_attempts() {
    let state = SessionState::new("fresh".to_string());
    assert_eq!(state.schema_version, 3);
    assert_eq!(state.validation_attempts, 0);
    assert!(state.block_origin.is_none());
}

#[test]
fn test_block_origin_serializes_snake_case() {
    let mut state = SessionState::new("block-origin".to_string());
    state.block_origin = Some(BlockOrigin::FinalValidation);
    let text = toml::to_string(&state).unwrap();
    assert!(
        text.contains(r#"block_origin = "final_validation""#),
        "block_origin must serialize as snake_case string: {text}"
    );
}

#[test]
fn test_block_origin_round_trip_all_variants() {
    for origin in [
        BlockOrigin::Brainstorm,
        BlockOrigin::SpecReview,
        BlockOrigin::SkipToImpl,
        BlockOrigin::Planning,
        BlockOrigin::PlanReview,
        BlockOrigin::Sharding,
        BlockOrigin::Implementation,
        BlockOrigin::Review,
        BlockOrigin::BuilderRecovery,
        BlockOrigin::GitGuard,
        BlockOrigin::FinalValidation,
    ] {
        let mut state = SessionState::new("rt".to_string());
        state.block_origin = Some(origin);
        let text = toml::to_string(&state).unwrap();
        let back: SessionState = toml::from_str(&text).unwrap();
        assert_eq!(back.block_origin, Some(origin));
    }
}

#[test]
fn test_block_origin_skipped_when_none() {
    let state = SessionState::new("no-origin".to_string());
    let text = toml::to_string(&state).unwrap();
    assert!(
        !text.contains("block_origin"),
        "block_origin must be omitted when None: {text}"
    );
}

#[test]
fn test_validation_attempts_field_persists() {
    let mut state = SessionState::new("attempts".to_string());
    state.validation_attempts = 3;
    let text = toml::to_string(&state).unwrap();
    let back: SessionState = toml::from_str(&text).unwrap();
    assert_eq!(back.validation_attempts, 3);
}

#[test]
fn test_for_stage_maps_known_stages() {
    assert_eq!(
        BlockOrigin::for_stage("coder"),
        Some(BlockOrigin::Implementation)
    );
    assert_eq!(
        BlockOrigin::for_stage("reviewer"),
        Some(BlockOrigin::Review)
    );
    assert_eq!(
        BlockOrigin::for_stage("recovery"),
        Some(BlockOrigin::BuilderRecovery)
    );
    assert_eq!(
        BlockOrigin::for_stage("brainstorm"),
        Some(BlockOrigin::Brainstorm)
    );
    assert_eq!(BlockOrigin::for_stage("unknown-stage"), None);
}

#[test]
fn test_session_state_v1_rejection() {
    with_temp_root(|| {
        // Manually write a v1 session file (no schema_version field)
        let dir = session_dir("test-v1-session");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        std::fs::write(
            &path,
            r#"
session_id = "test-v1-session"
current_phase = "IdeaInput"
"#,
        )
        .unwrap();

        let result = SessionState::load("test-v1-session");
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("schema v1") || err_msg.contains("archive"));
    });
}

#[test]
fn test_append_message() {
    with_temp_root(|| {
        let state = SessionState::new("test-msg-session".to_string());
        state.save().unwrap();

        let msg = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: "gpt-5".to_string(),
                vendor: "openai".to_string(),
            },
            text: "Exploring code".to_string(),
        };

        state.append_message(&msg).unwrap();

        // Verify file exists and contains the message
        let path = session_dir("test-msg-session").join("messages.toml");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Exploring code"));
    });
}

#[test]
fn test_load_messages() {
    with_temp_root(|| {
        let state = SessionState::new("test-load-msg".to_string());
        state.save().unwrap();

        let msg1 = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: "gpt-5".to_string(),
                vendor: "openai".to_string(),
            },
            text: "First".to_string(),
        };
        let msg2 = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::End,
            sender: MessageSender::System,
            text: "done in 1m".to_string(),
        };

        state.append_message(&msg1).unwrap();
        state.append_message(&msg2).unwrap();

        let loaded = SessionState::load_messages("test-load-msg").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text, "First");
        assert_eq!(loaded[1].text, "done in 1m");
    });
}

#[test]
fn test_load_messages_roundtrip_sender_field() {
    with_temp_root(|| {
        let state = SessionState::new("test-sender-msg".to_string());
        state.save().unwrap();
        let dir = session_dir("test-sender-msg");
        let path = dir.join("messages.toml");
        std::fs::write(
            &path,
            r#"[[messages]]
ts = "2026-04-24T00:00:00Z"
run_id = 1
kind = "Brief"
text = "hello"

[messages.sender.Agent]
model = "gpt-5"
vendor = "openai"
"#,
        )
        .unwrap();

        let loaded = SessionState::load_messages("test-sender-msg").unwrap();
        assert_eq!(loaded.len(), 1);

        assert_eq!(
            loaded[0].sender,
            MessageSender::Agent {
                model: "gpt-5".to_string(),
                vendor: "openai".to_string(),
            }
        );
    });
}

#[test]
fn test_load_messages_roundtrip_started_message() {
    with_temp_root(|| {
        let state = SessionState::new("test-started-msg".to_string());
        state.save().unwrap();
        let dir = session_dir("test-started-msg");
        let path = dir.join("messages.toml");
        std::fs::write(
            &path,
            r#"[[messages]]
ts = "2026-04-24T00:00:00Z"
run_id = 1
kind = "Started"
sender = "System"
text = "agent started · gpt-5 (openai)"
"#,
        )
        .unwrap();

        let loaded = SessionState::load_messages("test-started-msg").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].text, "agent started · gpt-5 (openai)");
        assert_eq!(loaded[0].kind, MessageKind::Started);
    });
}

#[test]
fn test_agent_text_messages_roundtrip_as_distinct_kind() {
    with_temp_root(|| {
        let state = SessionState::new("test-agent-text-msg".to_string());
        state.save().unwrap();
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id: 7,
            kind: MessageKind::AgentText,
            sender: MessageSender::Agent {
                model: "gpt-5".to_string(),
                vendor: "openai".to_string(),
            },
            text: "raw ACP text".to_string(),
        };

        state.append_message(&msg).unwrap();

        let loaded = SessionState::load_messages("test-agent-text-msg").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, MessageKind::AgentText);
        assert_eq!(loaded[0].text, "raw ACP text");
    });
}

#[test]
fn test_update_message_text_rewrites_existing_timestamped_message() {
    with_temp_root(|| {
        let state = SessionState::new("test-update-live-message".to_string());
        state.save().unwrap();
        let ts = chrono::Utc::now();
        let msg = Message {
            ts,
            run_id: 7,
            kind: MessageKind::AgentThought,
            sender: MessageSender::Agent {
                model: "model".to_string(),
                vendor: "vendor".to_string(),
            },
            text: "partial".to_string(),
        };
        state.append_message(&msg).unwrap();

        assert!(state.update_message_text(ts, "partial plus more").unwrap());

        let loaded = SessionState::load_messages("test-update-live-message").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].text, "partial plus more");
    });
}

#[test]
fn test_show_noninteractive_texts_defaults_and_serializes_false() {
    with_temp_root(|| {
        let state = SessionState::new("test-text-toggle-default".to_string());

        state.save().unwrap();

        let session_toml =
            std::fs::read_to_string(session_dir("test-text-toggle-default").join("session.toml"))
                .unwrap();
        assert!(
            session_toml.contains("show_noninteractive_texts = false"),
            "session.toml must persist the default toggle value: {session_toml}"
        );
        assert!(
            session_toml.contains("show_thinking_texts = false"),
            "session.toml must persist the default verbose toggle value: {session_toml}"
        );
        let loaded = SessionState::load("test-text-toggle-default").unwrap();
        assert!(!loaded.show_noninteractive_texts);
        assert!(!loaded.show_thinking_texts);
    });
}

#[test]
fn test_noninteractive_text_filter_only_hides_agent_text() {
    assert!(!MessageKind::AgentText.visible_with_agent_text_filter(false));
    assert!(MessageKind::AgentText.visible_with_agent_text_filter(true));
    assert!(!MessageKind::AgentThought.visible_with_filters(true, false));
    assert!(MessageKind::AgentThought.visible_with_filters(false, true));
    assert!(MessageKind::UserInput.visible_with_filters(false, false));
    assert!(MessageKind::Started.visible_with_agent_text_filter(false));
    assert!(MessageKind::Summary.visible_with_agent_text_filter(false));
    assert!(MessageKind::SummaryWarn.visible_with_agent_text_filter(false));
    assert!(MessageKind::End.visible_with_agent_text_filter(false));
}

#[test]
fn test_load_messages_rejects_old_jsonl() {
    with_temp_root(|| {
        let state = SessionState::new("test-corrupt-msg".to_string());
        state.save().unwrap();

        let dir = session_dir("test-corrupt-msg");
        let path = dir.join("messages.jsonl");
        std::fs::write(&path, r#"{"text":"old"}"#).unwrap();

        let err = SessionState::load_messages("test-corrupt-msg").unwrap_err();
        assert!(format!("{err:#}").contains("unsupported old JSON/JSONL"));
    });
}

#[test]
fn test_next_agent_run_id() {
    let mut state = SessionState::new("test-id".to_string());
    assert_eq!(state.next_agent_run_id(), 1);

    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });

    assert_eq!(state.next_agent_run_id(), 2);
}

#[test]
fn test_resume_one_running_live_window() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-resume".to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let result = state.resume_running_runs();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(1));
        assert_eq!(state.agent_runs[0].status, RunStatus::Running);
    });
}

#[test]
fn test_resume_one_running_missing_window() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-resume-missing".to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let result = state.resume_running_runs();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(1));
        assert_eq!(state.agent_runs[0].status, RunStatus::Running);
    });
}

#[test]
fn test_resume_multiple_running_runs() {
    let mut state = SessionState::new("test-resume-multi".to_string());
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "spec".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Spec]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });

    let result = state.resume_running_runs();

    assert!(result.is_err());
    let err = format!("{:?}", result.unwrap_err());
    assert!(err.contains("concurrent runs"));
}

#[test]
fn test_session_state_archived_defaults_false() {
    let state = SessionState::new("test-session".to_string());
    assert!(!state.archived);
}

#[test]
fn test_session_state_archived_persists() {
    let mut state = SessionState::new("test-session".to_string());
    state.archived = true;

    let toml = toml::to_string(&state).unwrap();
    assert!(toml.contains("archived = true"));

    let loaded: SessionState = toml::from_str(&toml).unwrap();
    assert!(loaded.archived);
}

#[test]
fn test_agent_runs_roundtrip() {
    let mut state = SessionState::new("test".to_string());
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes {
            yolo: true,
            cheap: true,
            interactive: false,
        },
        hostname: None,
        mount_device_id: None,
    });
    let toml = toml::to_string(&state).unwrap();
    let loaded: SessionState = toml::from_str(&toml).unwrap();
    assert_eq!(loaded.agent_runs.len(), 1);
    assert_eq!(loaded.agent_runs[0].id, 1);
    assert_eq!(loaded.agent_runs[0].stage, "brainstorm");
    assert_eq!(loaded.agent_runs[0].status, RunStatus::Done);
    assert_eq!(
        loaded.agent_runs[0].modes,
        LaunchModes {
            yolo: true,
            cheap: true,
            interactive: false,
        }
    );
}

#[test]
fn test_session_state_archived_defaults_false_on_deserialize() {
    let state = SessionState::new("test-session".to_string());
    let toml = toml::to_string(&state).unwrap();
    let loaded: SessionState = toml::from_str(&toml).unwrap();
    assert!(!loaded.archived);
}

#[test]
fn test_agent_runs_defaults_empty() {
    let state = SessionState::new("test".to_string());
    assert!(state.agent_runs.is_empty());
}

#[test]
fn test_schema_version_defaults_to_3() {
    let state = SessionState::new("test".to_string());
    assert_eq!(state.schema_version, 3);
}

#[test]
fn test_pipeline_item_create_minimal() {
    let item = PipelineItem {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(3),
        round: Some(2),
        status: PipelineItemStatus::Pending,
        title: Some("Normalize review artifacts".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
    };
    assert_eq!(item.id, 1);
    assert_eq!(item.stage, "coder");
    assert_eq!(item.task_id, Some(3));
    assert_eq!(item.status, PipelineItemStatus::Pending);
}

#[test]
fn test_pipeline_item_recovery_with_trigger() {
    let item = PipelineItem {
        id: 2,
        stage: "recovery".to_string(),
        task_id: None,
        round: None,
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: Some("human_blocked".to_string()),
        interactive: Some(true),
    };
    assert_eq!(item.trigger.as_deref(), Some("human_blocked"));
    assert_eq!(item.interactive, Some(true));
}

#[test]
fn test_pipeline_item_plan_review_with_mode() {
    let item = PipelineItem {
        id: 3,
        stage: "plan-review".to_string(),
        task_id: None,
        round: None,
        status: PipelineItemStatus::Running,
        title: None,
        mode: Some("recovery".to_string()),
        trigger: None,
        interactive: None,
    };
    assert_eq!(item.mode.as_deref(), Some("recovery"));
    assert_eq!(item.stage, "plan-review");
}

#[test]
fn test_pipeline_item_status_lifecycle_vs_verdict() {
    assert!(PipelineItemStatus::Pending.is_lifecycle());
    assert!(PipelineItemStatus::Running.is_lifecycle());
    assert!(PipelineItemStatus::Done.is_lifecycle());
    assert!(PipelineItemStatus::Failed.is_lifecycle());

    assert!(!PipelineItemStatus::Pending.is_verdict());
    assert!(!PipelineItemStatus::Done.is_verdict());

    assert!(PipelineItemStatus::Approved.is_verdict());
    assert!(PipelineItemStatus::Revise.is_verdict());
    assert!(PipelineItemStatus::HumanBlocked.is_verdict());
    assert!(PipelineItemStatus::AgentPivot.is_verdict());

    assert!(!PipelineItemStatus::Approved.is_lifecycle());
}

#[test]
fn test_pipeline_item_status_terminal() {
    assert!(!PipelineItemStatus::Pending.is_terminal());
    assert!(!PipelineItemStatus::Running.is_terminal());
    assert!(PipelineItemStatus::Done.is_terminal());
    assert!(PipelineItemStatus::Failed.is_terminal());
    assert!(PipelineItemStatus::Approved.is_terminal());
    assert!(PipelineItemStatus::Revise.is_terminal());
    assert!(PipelineItemStatus::HumanBlocked.is_terminal());
    assert!(PipelineItemStatus::AgentPivot.is_terminal());
}

#[test]
fn test_builder_push_pipeline_item_auto_id() {
    let mut builder = BuilderState::default();
    let item = PipelineItem {
        id: 0,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Pending,
        title: Some("First task".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
    };
    let id = builder.push_pipeline_item(item);
    assert_eq!(id, 1);
    assert_eq!(builder.pipeline_items.len(), 1);
    assert_eq!(builder.pipeline_items[0].id, 1);

    let item2 = PipelineItem {
        id: 0,
        stage: "reviewer".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    };
    let id2 = builder.push_pipeline_item(item2);
    assert_eq!(id2, 2);
    assert_eq!(builder.pipeline_items.len(), 2);
}

#[test]
fn test_builder_push_pipeline_item_explicit_id() {
    let mut builder = BuilderState::default();
    let item = PipelineItem {
        id: 42,
        stage: "sharding".to_string(),
        task_id: None,
        round: None,
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    };
    let id = builder.push_pipeline_item(item);
    assert_eq!(id, 42);
}

#[test]
fn test_builder_get_pipeline_item() {
    let mut builder = BuilderState::default();
    builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "coder".to_string(),
        task_id: Some(5),
        round: Some(1),
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    });
    let item = builder.get_pipeline_item(1).unwrap();
    assert_eq!(item.task_id, Some(5));
    assert!(builder.get_pipeline_item(99).is_none());
}

#[test]
fn test_builder_update_pipeline_status() {
    let mut builder = BuilderState::default();
    builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    });
    assert!(builder.update_pipeline_status(1, PipelineItemStatus::Running));
    assert_eq!(
        builder.get_pipeline_item(1).unwrap().status,
        PipelineItemStatus::Running
    );
    assert!(builder.update_pipeline_status(1, PipelineItemStatus::Approved));
    assert_eq!(
        builder.get_pipeline_item(1).unwrap().status,
        PipelineItemStatus::Approved
    );
    assert!(!builder.update_pipeline_status(99, PipelineItemStatus::Failed));
}

#[test]
fn test_builder_pipeline_items_by_stage() {
    let mut builder = BuilderState::default();
    for (stage, tid) in &[("coder", 1), ("reviewer", 1), ("coder", 2), ("recovery", 0)] {
        builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: stage.to_string(),
            task_id: if *tid > 0 { Some(*tid) } else { None },
            round: None,
            status: PipelineItemStatus::Pending,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
        });
    }
    assert_eq!(builder.pipeline_items_by_stage("coder").len(), 2);
    assert_eq!(builder.pipeline_items_by_stage("reviewer").len(), 1);
    assert_eq!(builder.pipeline_items_by_stage("recovery").len(), 1);
    assert_eq!(builder.pipeline_items_by_stage("brainstorm").len(), 0);
}

#[test]
fn test_builder_pending_and_running_items() {
    let mut builder = BuilderState::default();
    builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    });
    builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "coder".to_string(),
        task_id: Some(2),
        round: Some(1),
        status: PipelineItemStatus::Running,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    });
    builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "reviewer".to_string(),
        task_id: Some(3),
        round: Some(1),
        status: PipelineItemStatus::Done,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    });
    assert_eq!(builder.pending_pipeline_items().len(), 1);
    assert_eq!(builder.running_pipeline_items().len(), 1);
    assert_eq!(builder.pending_pipeline_items()[0].task_id, Some(1));
    assert_eq!(builder.running_pipeline_items()[0].task_id, Some(2));
}

#[test]
fn test_pipeline_item_toml_roundtrip() {
    let item = PipelineItem {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(3),
        round: Some(2),
        status: PipelineItemStatus::Pending,
        title: Some("Normalize review artifacts".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
    };

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Wrapper {
        pipeline_items: Vec<PipelineItem>,
    }

    let wrapper = Wrapper {
        pipeline_items: vec![item.clone()],
    };
    let toml_str = toml::to_string_pretty(&wrapper).unwrap();
    let loaded: Wrapper = toml::from_str(&toml_str).unwrap();
    assert_eq!(loaded.pipeline_items.len(), 1);
    assert_eq!(loaded.pipeline_items[0], item);
}

#[test]
fn test_pipeline_item_toml_skip_none_fields() {
    let item = PipelineItem {
        id: 1,
        stage: "coder".to_string(),
        task_id: None,
        round: None,
        status: PipelineItemStatus::Pending,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    };

    let toml_str = toml::to_string_pretty(&item).unwrap();
    assert!(!toml_str.contains("task_id"));
    assert!(!toml_str.contains("round"));
    assert!(!toml_str.contains("title"));
    assert!(!toml_str.contains("mode"));
    assert!(!toml_str.contains("trigger"));
    assert!(!toml_str.contains("interactive"));
}

#[test]
fn test_pipeline_item_toml_recovery_with_trigger() {
    let toml_str = r#"
id = 5
stage = "recovery"
status = "pending"
trigger = "agent_pivot"
interactive = false
"#;
    let item: PipelineItem = toml::from_str(toml_str).unwrap();
    assert_eq!(item.stage, "recovery");
    assert_eq!(item.trigger.as_deref(), Some("agent_pivot"));
    assert_eq!(item.interactive, Some(false));
    assert_eq!(item.status, PipelineItemStatus::Pending);
}

#[test]
fn test_pipeline_item_status_serde_snake_case() {
    let item = PipelineItem {
        id: 1,
        stage: "reviewer".to_string(),
        task_id: Some(2),
        round: Some(1),
        status: PipelineItemStatus::HumanBlocked,
        title: None,
        mode: None,
        trigger: None,
        interactive: None,
    };
    let toml_str = toml::to_string_pretty(&item).unwrap();
    assert!(toml_str.contains("human_blocked"));

    let loaded: PipelineItem = toml::from_str(&toml_str).unwrap();
    assert_eq!(loaded.status, PipelineItemStatus::HumanBlocked);
}

#[test]
fn test_pipeline_items_persist_in_session() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-pipeline".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Pending,
            title: Some("First task".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: None,
            status: PipelineItemStatus::Running,
            title: None,
            mode: Some("recovery".to_string()),
            trigger: Some("human_blocked".to_string()),
            interactive: Some(true),
        });

        state.save().unwrap();
        let loaded = SessionState::load("test-pipeline").unwrap();

        assert_eq!(loaded.builder.pipeline_items.len(), 2);
        assert_eq!(loaded.builder.pipeline_items[0].stage, "coder");
        assert_eq!(loaded.builder.pipeline_items[0].task_id, Some(1));
        assert_eq!(
            loaded.builder.pipeline_items[0].status,
            PipelineItemStatus::Pending
        );
        assert_eq!(
            loaded.builder.pipeline_items[0].title.as_deref(),
            Some("First task")
        );

        assert_eq!(loaded.builder.pipeline_items[1].stage, "recovery");
        assert_eq!(
            loaded.builder.pipeline_items[1].trigger.as_deref(),
            Some("human_blocked")
        );
        assert_eq!(loaded.builder.pipeline_items[1].interactive, Some(true));
    });
}

#[test]
fn test_pipeline_items_update_then_persist() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-update-pipe".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(5),
            round: Some(1),
            status: PipelineItemStatus::Pending,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
        });
        state
            .builder
            .update_pipeline_status(1, PipelineItemStatus::Approved);
        state.save().unwrap();

        let loaded = SessionState::load("test-update-pipe").unwrap();
        assert_eq!(
            loaded.builder.pipeline_items[0].status,
            PipelineItemStatus::Approved
        );
    });
}

#[test]
fn test_pipeline_item_default_status_is_pending() {
    assert_eq!(PipelineItemStatus::default(), PipelineItemStatus::Pending);
}

#[test]
fn test_pipeline_all_verdict_values_roundtrip() {
    for (input, expected) in [
        ("\"approved\"", PipelineItemStatus::Approved),
        ("\"revise\"", PipelineItemStatus::Revise),
        ("\"human_blocked\"", PipelineItemStatus::HumanBlocked),
        ("\"agent_pivot\"", PipelineItemStatus::AgentPivot),
    ] {
        let status: PipelineItemStatus = toml::from_str(&format!("status = {input}\n"))
            .map(|w: std::collections::HashMap<String, PipelineItemStatus>| w["status"])
            .unwrap();
        assert_eq!(status, expected);
    }
}

#[test]
fn test_resume_hostname_mismatch_marks_failed_unverified() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-hostname-mismatch".to_string());
        let current_hostname = SessionState::capture_hostname();
        let different_hostname = Some("different-host".to_string());

        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: if current_hostname.is_some() {
                different_hostname
            } else {
                Some("some-host".to_string())
            },
            mount_device_id: None,
        });

        let result = state.resume_running_runs();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
        assert_eq!(state.agent_runs[0].status, RunStatus::FailedUnverified);
        assert!(state.agent_runs[0].error.is_some());
        assert!(
            state.agent_runs[0]
                .error
                .as_ref()
                .unwrap()
                .contains("hostname mismatch")
        );
        assert!(
            state.agent_runs[0].ended_at.is_some(),
            "ended_at must be set when a Running run is finalized as FailedUnverified"
        );
        let messages = SessionState::load_messages(&state.session_id).unwrap();
        assert!(
            messages.iter().any(|m| m.kind == MessageKind::End
                && m.run_id == 1
                && m.text.starts_with("failed-unverified in")),
            "expected an End message with the failed-unverified duration prefix; got {:?}",
            messages
                .iter()
                .map(|m| (&m.kind, &m.text))
                .collect::<Vec<_>>()
        );
    });
}

#[test]
fn test_resume_mount_device_mismatch_marks_failed_unverified() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-mount-mismatch".to_string());
        let current_device = SessionState::capture_mount_device_id();

        // Only run this test if we can capture a device ID (Unix systems)
        if current_device.is_none() {
            return;
        }

        let different_device = current_device.map(|d| d.wrapping_add(1));

        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: different_device,
        });

        let result = state.resume_running_runs();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
        assert_eq!(state.agent_runs[0].status, RunStatus::FailedUnverified);
        assert!(state.agent_runs[0].error.is_some());
        assert!(
            state.agent_runs[0]
                .error
                .as_ref()
                .unwrap()
                .contains("mount device mismatch")
        );
    });
}

#[test]
fn test_resume_same_host_identity_preserves_running() {
    with_temp_root(|| {
        let mut state = SessionState::new("test-same-host".to_string());
        let current_hostname = SessionState::capture_hostname();
        let current_device = SessionState::capture_mount_device_id();

        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: current_hostname,
            mount_device_id: current_device,
        });

        let result = state.resume_running_runs();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(1));
        assert_eq!(state.agent_runs[0].status, RunStatus::Running);
        assert!(state.agent_runs[0].error.is_none());
    });
}

#[test]
fn test_run_record_backward_compat_missing_effort() {
    let json = r#"{
            "id": 42,
            "stage": "coder",
            "task_id": 1,
            "round": 1,
            "attempt": 1,
            "model": "claude-opus-4-7",
            "vendor": "anthropic",
            "window_name": "[Coder r1]",
            "started_at": "2025-01-01T00:00:00Z",
            "ended_at": null,
            "status": "Running",
            "error": null
        }"#;
    let record: RunRecord = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(record.effort, EffortLevel::Normal);
    assert_eq!(record.modes, LaunchModes::default());

    let round_tripped = serde_json::to_string(&record).expect("should serialize");
    let record2: RunRecord = serde_json::from_str(&round_tripped).expect("should round-trip");
    assert_eq!(record2.effort, EffortLevel::Normal);
    assert_eq!(record2.modes, LaunchModes::default());
    assert_eq!(record2.id, 42);
}
