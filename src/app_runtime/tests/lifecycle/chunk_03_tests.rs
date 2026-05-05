use super::*;

#[test]
fn interactive_run_arrows_navigate_when_input_is_not_active() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-run-arrows".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);
        app.input_mode = false;
        let start = app.selected;

        app.handle_key(key(crossterm::event::KeyCode::Down));

        assert!(
            app.selected > start,
            "Down should move focus while the textbox is inactive"
        );
        assert!(!app.input_mode);
    });
}

#[test]
fn pending_guard_modal_ctrl_c_stops_running_agent_without_quitting() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-quit", 32));

        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert!(!app.handle_key(ctrl_c));
        assert!(app.state.pending_guard_decision.is_some());
        let events_path = session_state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(
            events.contains("agent_stopped_by_user: run_id=32"),
            "Ctrl+C should always route through stop_running_agent while a run is active"
        );
    });
}

#[test]
fn idle_ctrl_c_quits_when_no_agent_is_running() {
    with_temp_root(|| {
        let mut app = idle_app(SessionState::new("idle-ctrl-c-quits".to_string()));
        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );

        assert!(app.handle_key(ctrl_c));
    });
}

#[test]
fn paused_review_modal_ctrl_c_quits_without_running_agent() {
    with_temp_root(|| {
        let mut state = SessionState::new("paused-modal-ctrl-c-quits".to_string());
        state.current_phase = Phase::SpecReviewPaused;
        let mut app = idle_app(state);
        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );

        assert!(app.handle_key(ctrl_c));
    });
}

#[test]
fn pending_guard_modal_q_still_follows_quit_path() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-q-quit", 32));

        assert!(app.handle_key(key(crossterm::event::KeyCode::Char('q'))));
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn pending_guard_modal_escape_matches_q_quit_path() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-esc", 34));

        assert!(app.handle_key(key(crossterm::event::KeyCode::Esc)));
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn pending_guard_modal_consumes_unrelated_keys() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-consume", 33));
        app.confirm_back = true;

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('x')));

        assert!(!should_quit);
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn palette_back_rewinds_without_second_confirmation() {
    with_temp_root(|| {
        let mut app = mk_app(mk_state_with_runs());

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "back".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        assert!(!app.confirm_back);
    });
}

#[test]
fn palette_retry_clears_selected_task_attempt_logs_and_relaunches() {
    with_temp_root(|| {
        let session_id = "palette-retry-selected-task";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.builder.recovery_trigger_task_id = Some(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
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
            stage: "reviewer".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "gemini-2.5-pro".to_string(),
            vendor: "gemini".to_string(),
            window_name: "[Round 1 Reviewer]".to_string(),
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
            id: 3,
            stage: "recovery".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Recovery]".to_string(),
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
        let removed_run = state.agent_runs[0].clone();
        state.save().expect("save");
        state
            .append_message(&crate::state::Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: crate::state::MessageSender::System,
                text: "attempt 1 failed".to_string(),
            })
            .expect("append message");

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        write_finish_stamp_for_run(&app, &removed_run, 1, "");
        std::fs::write(app.live_summary_path_for(&removed_run), "old summary").expect("summary");
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Task 1");

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.agent_runs.len(), 1);
        let fresh = &app.state.agent_runs[0];
        assert_eq!(fresh.stage, "coder");
        assert_eq!(fresh.task_id, Some(1));
        assert_eq!(fresh.attempt, 1);
        assert_eq!(fresh.status, RunStatus::Running);
        assert!(!app.live_summary_path_for(&removed_run).exists());
        let messages = SessionState::load_messages(session_id).expect("messages");
        assert!(
            messages
                .iter()
                .all(|message| message.text != "attempt 1 failed")
        );
    });
}

#[test]
fn palette_retry_is_available_from_builder_loop_focus() {
    with_temp_root(|| {
        let session_id = "palette-retry-loop-focus";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
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

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Loop");

        assert!(
            app.palette_commands()
                .iter()
                .any(|command| command.name == "retry"),
            ":retry should be available when the current builder task is selected by context"
        );

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.agent_runs.len(), 1);
        assert_eq!(app.state.agent_runs[0].attempt, 1);
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Running);
    });
}

#[test]
fn palette_retry_clears_brainstorm_attempt_logs_and_relaunches() {
    with_temp_root(|| {
        let session_id = "palette-retry-brainstorm";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("draft the spec".to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Brainstorm] gpt-5".to_string(),
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
        let removed_run = state.agent_runs[0].clone();
        state.save().expect("save");
        state
            .append_message(&crate::state::Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: crate::state::MessageSender::System,
                text: "brainstorm failed".to_string(),
            })
            .expect("append message");

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            1,
            10,
            10,
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

        write_finish_stamp_for_run(&app, &removed_run, 1, "");
        std::fs::write(app.live_summary_path_for(&removed_run), "old summary").expect("summary");
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Brainstorm");

        assert!(
            app.palette_commands()
                .iter()
                .any(|command| command.name == "retry"),
            ":retry should be available for Brainstorm focus"
        );

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.agent_runs.len(), 1);
        let fresh = &app.state.agent_runs[0];
        assert_eq!(fresh.stage, "brainstorm");
        assert_eq!(fresh.attempt, 1);
        assert_eq!(fresh.status, RunStatus::Running);
        assert!(!app.live_summary_path_for(&removed_run).exists());
        let messages = SessionState::load_messages(session_id).expect("messages");
        assert!(
            messages
                .iter()
                .all(|message| message.text != "brainstorm failed")
        );
    });
}

#[test]
fn palette_retry_is_available_from_non_task_stage_focus() {
    with_temp_root(|| {
        let mut state = SessionState::new("palette-retry-stage-focus".to_string());
        for (id, stage) in [
            (1, "brainstorm"),
            (2, "spec-review"),
            (3, "planning"),
            (4, "plan-review"),
            (5, "sharding"),
        ] {
            state.agent_runs.push(RunRecord {
                id,
                stage: stage.to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: format!("[{stage}]"),
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
        }
        let mut app = idle_app(state);

        for label in [
            "Brainstorm",
            "Spec Review",
            "Planning",
            "Plan Review",
            "Sharding",
        ] {
            app.selected = row_index(&app, label);
            assert!(
                app.palette_commands()
                    .iter()
                    .any(|command| command.name == "retry"),
                ":retry should be available for {label} focus"
            );
        }
    });
}

#[test]
fn running_palette_shows_stop_retry_and_no_legacy_aliases() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-palette-commands".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let app = mk_app(state);

        let stop = app
            .palette_commands()
            .into_iter()
            .find(|command| command.name == "stop")
            .expect("stop command");
        assert_eq!(stop.help, "Stop the running agent without retry");
        assert!(
            stop.aliases.is_empty(),
            "legacy stop aliases should be removed"
        );

        let retry = app
            .palette_commands()
            .into_iter()
            .find(|command| command.name == "retry")
            .expect("retry command");
        assert_eq!(retry.help, "Stop and retry the running agent");

        let commands = app.palette_commands();
        let names = commands
            .iter()
            .flat_map(|command| {
                std::iter::once(command.name).chain(command.aliases.iter().copied())
            })
            .collect::<Vec<_>>();
        assert!(!names.contains(&"kill"));
        assert!(!names.contains(&"cancel"));
    });
}

#[test]
fn running_palette_retry_stops_current_run_with_retry_marker() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-palette-retry".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let run = make_brainstorm_run(7);
        state.agent_runs.push(run);
        let mut app = mk_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }

        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        let events_path = session_state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(
            events.contains("agent_retry_requested_by_user: run_id=7"),
            "running :retry should log the forced-retry marker"
        );
    });
}

#[test]
fn conflicting_running_termination_request_keeps_first_intent_and_surfaces_status() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-termination-first-wins".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "stop".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert_eq!(
            app.pending_termination,
            Some(PendingTermination::new_stop_only(7))
        );
        let status = app.status_line.borrow().render().expect("status flash");
        assert!(
            status
                .to_string()
                .contains("Termination already pending: keeping stop without retry.")
        );

        let events_path = session_state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(events.contains("agent_stopped_by_user: run_id=7"));
        assert!(!events.contains("agent_retry_requested_by_user: run_id=7"));
    });
}

#[test]
fn idle_enter_retries_selected_target() {
    with_temp_root(|| {
        let session_id = "idle-enter-retry-selected-task";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
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
        let removed_run = state.agent_runs[0].clone();
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        write_finish_stamp_for_run(&app, &removed_run, 1, "");
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Task 1");

        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert_eq!(app.state.agent_runs.len(), 1);
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Running);
        assert_eq!(app.state.agent_runs[0].stage, "coder");
    });
}

#[test]
fn bare_enter_while_running_does_not_trigger_retry() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-enter-no-retry".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);
        let before = app
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "brainstorm")
            .count();

        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        let after = app
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "brainstorm")
            .count();
        assert_eq!(after, before, "bare Enter must not trigger running retry");
    });
}

#[test]
fn quit_command_with_running_agent_opens_confirmation_modal() {
    with_temp_root(|| {
        let mut state = SessionState::new("quit-running-modal".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "quit".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit, "quit should wait for post-stop finalization");
        assert_eq!(app.active_modal(), Some(ModalKind::QuitRunningAgent));
    });
}

#[test]
fn quit_confirmation_cancel_leaves_run_active() {
    with_temp_root(|| {
        let mut state = SessionState::new("quit-running-modal-cancel".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);
        app.pending_quit_confirmation_run_id = Some(7);

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('q')));

        assert!(!should_quit);
        assert_eq!(app.active_modal(), None);
        assert!(app.has_running_agent());
    });
}
