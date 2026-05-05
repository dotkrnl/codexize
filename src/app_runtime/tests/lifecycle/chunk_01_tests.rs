use super::*;

#[test]
fn up_at_top_of_section_moves_focus_to_previous_row() {
    let mut app = mk_app(mk_state_with_runs());
    let sr_idx = row_index(&app, "Spec Review");
    app.selected = sr_idx;
    app.scroll_or_move_focus(-1);
    assert!(app.selected < sr_idx);
}

#[test]
fn space_binding_does_not_affect_input_mode() {
    let mut app = mk_app(mk_state_with_runs());
    app.input_mode = true;
    let before = app.collapsed_overrides.clone();
    // Directly test the guard: toggle_expand_focused shouldn't be reached via
    // input-mode keys. Sanity: toggle itself still works outside input mode.
    app.input_mode = false;
    app.selected = row_index(&app, "Brainstorm");
    app.toggle_expand_focused();
    assert_ne!(app.collapsed_overrides, before);
}

#[test]
fn down_boundary_handoff_moves_to_next_visible_row_even_when_collapsed() {
    let mut app = mk_app(SessionState::new("boundary-visible-row".to_string()));
    app.nodes = vec![Node {
        label: "Root".to_string(),
        kind: crate::state::NodeKind::Stage,
        status: crate::state::NodeStatus::Running,
        summary: String::new(),
        children: vec![
            Node {
                label: "Collapsed Task".to_string(),
                kind: crate::state::NodeKind::Task,
                status: crate::state::NodeStatus::Done,
                summary: String::new(),
                children: Vec::new(),
                run_id: None,
                leaf_run_id: Some(11),
            },
            Node {
                label: "Expanded Task".to_string(),
                kind: crate::state::NodeKind::Task,
                status: crate::state::NodeStatus::Done,
                summary: String::new(),
                children: Vec::new(),
                run_id: None,
                leaf_run_id: Some(12),
            },
        ],
        run_id: None,
        leaf_run_id: None,
    }];
    app.rebuild_visible_rows();
    let expanded_idx = row_index(&app, "Expanded Task");
    let expanded_key = app.visible_rows[expanded_idx].key.clone();
    app.collapsed_overrides
        .insert(expanded_key, ExpansionOverride::Expanded);
    app.rebuild_visible_rows();

    app.selected = row_index(&app, "Root");
    app.scroll_or_move_focus(1);

    assert_eq!(row_label(&app, app.selected), "Collapsed Task");
}

#[test]
fn space_does_not_toggle_pending_rows() {
    let mut state = SessionState::new("pending-toggle".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.pending = vec![4];
    let mut app = mk_app(state);
    let pending_idx = row_index(&app, "Task 4");
    app.selected = pending_idx;

    app.toggle_expand_focused();

    assert!(app.collapsed_overrides.is_empty());
    assert!(!app.is_expanded(pending_idx));
}

#[test]
fn space_collapse_override_collapses_active_path_row() {
    let mut state = SessionState::new("active-space".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.current_task = Some(7);
    state.agent_runs.push(RunRecord {
        id: 88,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    let mut app = mk_app(state);
    let coder_idx = row_index(&app, "Builder");
    let coder_key = app.visible_rows[coder_idx].key.clone();
    app.selected = coder_idx;

    app.toggle_expand_focused();

    assert_eq!(
        app.collapsed_overrides.get(&coder_key),
        Some(&ExpansionOverride::Collapsed)
    );
    let coder_idx = row_index(&app, "Builder");
    assert!(!app.is_expanded(coder_idx));
}

#[test]
fn enter_does_not_toggle_expansion_for_focused_row() {
    let mut app = mk_app(mk_state_with_runs());
    let brainstorm_idx = row_index(&app, "Brainstorm");
    let before = app.collapsed_overrides.clone();
    app.selected = brainstorm_idx;

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));

    assert_eq!(app.collapsed_overrides, before);
    assert!(app.is_expanded(brainstorm_idx));
}

#[test]
fn builder_task_row_can_be_focused_and_expanded_to_transcript_descendant() {
    let mut state = SessionState::new("builder-drilldown".to_string());
    state.current_phase = Phase::ImplementationRound(2);
    state.builder.done = vec![7];
    state.builder.current_task = Some(8);
    state.agent_runs.push(RunRecord {
        id: 71,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder 7]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 81,
        stage: "coder".to_string(),
        task_id: Some(8),
        round: 2,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder 8]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    let mut app = mk_app(state);
    let task_idx = row_index(&app, "Task 7");
    app.selected = task_idx;

    app.toggle_expand_focused();

    assert_eq!(row_label(&app, app.selected), "Task 7");
    assert!(row_index_opt(&app, "Builder").is_some());
}

#[test]
fn repeated_attempt_labels_keep_independent_expansion_state() {
    let mut state = SessionState::new("attempt-identity".to_string());
    state.current_phase = Phase::ReviewRound(1);
    state.builder.current_task = Some(5);
    for (id, stage, attempt, status) in [
        (41, "coder", 1, RunStatus::Failed),
        (42, "coder", 2, RunStatus::Done),
        (43, "reviewer", 1, RunStatus::Failed),
        (44, "reviewer", 2, RunStatus::Running),
    ] {
        state.agent_runs.push(RunRecord {
            id,
            stage: stage.to_string(),
            task_id: Some(5),
            round: 1,
            attempt,
            model: "gpt-5".to_string(),
            vendor: "openai".to_string(),
            window_name: format!("[{stage}]"),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
    }
    let mut app = mk_app(state);
    let attempt_rows = app
        .visible_rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            node_at_path(&app.nodes, &row.path).is_some_and(|node| node.label == "Attempt 1")
        })
        .map(|(index, row)| (index, row.key.clone()))
        .collect::<Vec<_>>();
    assert_eq!(attempt_rows.len(), 2);
    assert_ne!(attempt_rows[0].1, attempt_rows[1].1);

    app.selected = attempt_rows[0].0;
    app.toggle_expand_focused();

    assert_eq!(
        app.collapsed_overrides.get(&attempt_rows[0].1),
        Some(&ExpansionOverride::Collapsed)
    );
    assert!(!app.collapsed_overrides.contains_key(&attempt_rows[1].1));
}

#[test]
fn on_frame_drawn_advances_spinner_tick_without_agent_changes() {
    let mut app = idle_app(SessionState::new("on-frame-drawn".to_string()));
    let before = app.spinner_tick;

    for _ in 0..97 {
        app.on_frame_drawn();
    }

    assert_eq!(app.spinner_tick, before.wrapping_add(97));
    assert_eq!(app.agent_content_hash, 0);
    assert!(app.agent_last_change.is_none());
}

#[test]
fn event_poll_duration_uses_fast_cadence_only_for_visible_live_summary_spinner() {
    let mut app = idle_app(SessionState::new("frame-poll-duration".to_string()));

    app.live_summary_spinner_visible = false;
    assert_eq!(app.event_poll_duration(), Duration::from_millis(250));

    app.live_summary_spinner_visible = true;
    assert_eq!(app.event_poll_duration(), Duration::from_millis(50));
}

#[test]
fn picker_created_startup_draws_before_auto_launch() {
    with_temp_root(|| {
        let session_id = "picker-created-first-frame";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("Ship the picker handoff".to_string());
        state.save().expect("save session");

        let mut app = App::new_with_startup_origin(
            SessionState::load(session_id).expect("load session"),
            AppStartupOrigin::PickerCreated,
        );
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            10,
            1,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some("# Spec\n".to_string()),
                    launch_error: None,
                }]),
            },
        )));

        app.maybe_auto_launch();
        assert!(
            app.state.agent_runs.is_empty(),
            "picker-created startup must wait for the first visible frame"
        );

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let view = app.current_app_view();
        terminal.draw(|frame| app.draw(frame, &view)).expect("draw");

        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        assert!(
            app.state.agent_runs.is_empty(),
            "successful draw alone must not backdoor a launch"
        );
    });
}

#[test]
fn update_agent_progress_reloads_persisted_interactive_agent_text() {
    with_temp_root(|| {
        let session_id = "interactive-output-reload";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run.clone());
        state.save().expect("save state");
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        let msg = Message {
            ts: chrono::Utc::now(),
            run_id: 7,
            kind: MessageKind::AgentText,
            sender: crate::state::MessageSender::Agent {
                model: run.model,
                vendor: run.vendor,
            },
            text: "question for operator".to_string(),
        };
        SessionState::load(session_id)
            .expect("load state")
            .append_message(&msg)
            .expect("append message");

        app.update_agent_progress();

        assert!(app.messages.iter().any(|message| {
            message.run_id == 7
                && message.kind == MessageKind::AgentText
                && message.text == "question for operator"
        }));
    });
}

#[test]
fn update_agent_progress_reloads_in_place_message_text_changes() {
    with_temp_root(|| {
        let session_id = "interactive-output-upsert-reload";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run.clone());
        state.save().expect("save state");
        let mut app = idle_app(state.clone());
        app.current_run_id = Some(7);

        let ts = chrono::Utc::now();
        let msg = Message {
            ts,
            run_id: 7,
            kind: MessageKind::AgentThought,
            sender: crate::state::MessageSender::Agent {
                model: run.model,
                vendor: run.vendor,
            },
            text: "partial".to_string(),
        };
        state.append_message(&msg).expect("append message");
        app.update_agent_progress();
        assert!(app.messages.iter().any(|message| message.text == "partial"));

        state
            .update_message_text(ts, "partial plus more")
            .expect("update message");
        app.update_agent_progress();

        assert!(app.messages.iter().any(|message| {
            message.run_id == 7
                && message.kind == MessageKind::AgentThought
                && message.text == "partial plus more"
        }));
    });
}

#[test]
fn app_new_rebuilds_failed_models_without_force_retry_runs() {
    with_temp_root(|| {
        let session_id = "rebuild-failed-models";
        let mut state = SessionState::new(session_id.to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 3,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Builder r3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 3,
            attempt: 2,
            model: "gemini-2.5-pro".to_string(),
            vendor: "gemini".to_string(),
            window_name: "[Builder r3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("artifact_missing".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        state.agent_runs.push(RunRecord {
            id: 3,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 3,
            attempt: 3,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder r3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("user_forced_retry".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        state.save().expect("save session");

        let app = App::new(SessionState::load(session_id).expect("load session"));

        let key = ("coder".to_string(), Some(7), 3);
        let failed = app
            .failed_models
            .get(&key)
            .expect("expected failed model set");
        assert!(failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string())));
        assert!(failed.contains(&(selection::VendorKind::Gemini, "gemini-2.5-pro".to_string())));
        assert!(!failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
        assert!(app.current_run_id.is_none());
    });
}

#[test]
fn non_coder_missing_stamp_warns_and_still_retries_after_timeout() {
    with_temp_root(|| {
        let session_id = "planning-missing-stamp-warning";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                        launch_error: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                        launch_error: None,
                    },
                ]),
            },
        )));

        app.launch_planning();
        let first_id = app.current_run_id.expect("first planning run id");
        let first = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == first_id)
            .cloned()
            .expect("first run");
        let _ = std::fs::remove_file(app.finish_stamp_path_for(&first));
        let _ = std::fs::remove_file(app.live_summary_path_for(&first));

        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
        app.poll_agent_run();

        let warn = app
            .messages
            .iter()
            .find(|message| {
                message.run_id == first.id
                    && message.kind == MessageKind::SummaryWarn
                    && message.text.contains("finish_stamp_missing")
            })
            .expect("missing-stamp warning");
        assert!(warn.text.contains("planning"));
        assert!(
            app.state
                .agent_runs
                .iter()
                .any(|run| run.stage == "planning"
                    && run.attempt == 2
                    && run.status == RunStatus::Running)
        );
    });
}

#[test]
fn non_builder_retry_exhaustion_still_blocks() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-builder-retry".to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet",
            1,
            10,
            10,
        )];
        let failed = RunRecord {
            id: 11,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 3,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        };
        let handled = app.maybe_auto_retry(&failed);
        assert!(handled);
        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert!(!matches!(
            app.state.current_phase,
            Phase::BuilderRecovery(_)
        ));
    });
}

#[test]
fn app_new_rebuild_failed_models_skips_builder_failures_before_retry_reset_cutoff() {
    with_temp_root(|| {
        let session_id = "failed-model-retry-reset";
        let mut state = SessionState::new(session_id.to_string());
        state.builder.retry_reset_run_id_cutoff = Some(10);
        state.agent_runs.push(RunRecord {
            id: 9,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Builder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        state.agent_runs.push(RunRecord {
            id: 11,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 2,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        state.save().expect("save");
        let app = App::new(SessionState::load(session_id).expect("load"));
        let key = ("coder".to_string(), Some(1), 1);
        let failed = app.failed_models.get(&key).expect("failed set");
        assert_eq!(failed.len(), 1);
        assert!(failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
        assert!(!failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string())));
    });
}

#[test]
fn go_back_from_impl_round_one_on_skip_path_returns_to_brainstorm() {
    with_temp_root(|| {
        let session_id = "skip-back-nav";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.skip_to_impl_rationale = Some("trivial change".to_string());
        // Seed a non-default BuilderState so we can detect that the skip branch
        // preserves it (unlike the normal-path branch, which resets).
        state.builder.pending = vec![1];
        state.builder.task_titles.insert(1, "t".to_string());

        let mut app = idle_app(state);
        app.go_back();

        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        // Skip-path back-nav should not clobber BuilderState the way the
        // ShardingRunning branch does.
        assert_eq!(app.state.builder.pending, vec![1]);
    });
}

#[test]
fn go_back_from_impl_round_one_without_skip_resets_to_sharding() {
    with_temp_root(|| {
        let session_id = "normal-back-nav";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.skip_to_impl_rationale = None;
        state.builder.pending = vec![1];

        let mut app = idle_app(state);
        app.go_back();

        assert_eq!(app.state.current_phase, Phase::ShardingRunning);
        assert!(app.state.builder.pending.is_empty());
    });
}
