use super::*;

#[test]
fn brainstorm_selection_uses_idea_task_kind() {
    let models = vec![
        sample_model("idea-first", 1, 2),
        sample_model("build-first", 2, 1),
    ];

    let chosen = App::select_brainstorm_model(&models).expect("expected brainstorm model");

    assert_eq!(chosen.name, "idea-first");
}

#[test]
fn interactive_launch_opens_matching_run_split_immediately() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-launch-split".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut app = idle_app(state);
        app.split_target = Some(super::split::SplitTarget::Idea);

        app.start_run_tracking(
            "brainstorm",
            None,
            1,
            "model".to_string(),
            "vendor".to_string(),
            "[Brainstorm]".to_string(),
            EffortLevel::Normal,
            crate::state::LaunchModes::default(),
            std::path::PathBuf::from("prompts/brainstorm.md"),
        );

        let run_id = app.current_run_id.expect("run id");
        assert_eq!(
            app.split_target,
            Some(super::split::SplitTarget::Run(run_id)),
            "interactive launch should replace any open split with the new run split"
        );
        assert!(
            !app.input_mode,
            "launch auto-open should not focus input until the run is waiting for input"
        );
    });
}

#[test]
fn coder_retry_loop_uses_distinct_models_until_success() {
    with_temp_root(|| {
        let session_id = "coder-retry-loop";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Claude, "claude-sonnet", 10, 1, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 3, 10),
        ];
        let harness = std::sync::Arc::new(std::sync::Mutex::new(TestLaunchHarness {
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
                TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some("abc123".to_string()),
                    launch_error: None,
                },
            ]),
        }));
        app.test_launch_harness = Some(harness);

        app.launch_coder();
        for _ in 0..6 {
            if app.current_run_id.is_none() {
                break;
            }
            app.poll_agent_run();
        }

        assert!(app.current_run_id.is_none());
        assert_eq!(app.state.agent_runs.len(), 3);
        assert_eq!(app.state.agent_runs[0].attempt, 1);
        assert_eq!(app.state.agent_runs[1].attempt, 2);
        assert_eq!(app.state.agent_runs[2].attempt, 3);
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
        assert_eq!(app.state.agent_runs[1].status, RunStatus::Failed);
        assert_eq!(app.state.agent_runs[2].status, RunStatus::Done);
        assert_eq!(app.state.agent_runs[0].error.as_deref(), Some("exit(1)"));
        assert_eq!(app.state.agent_runs[1].error.as_deref(), Some("exit(1)"));
        assert_eq!(app.state.agent_runs[0].model, "claude-sonnet");
        assert_eq!(app.state.agent_runs[1].model, "gemini-2.5-pro");
        assert_eq!(app.state.agent_runs[2].model, "gpt-5");
        assert_eq!(app.state.current_phase, Phase::ReviewRound(1));

        let end_texts = app
            .messages
            .iter()
            .filter(|message| message.kind == MessageKind::End)
            .map(|message| message.text.clone())
            .collect::<Vec<_>>();
        assert!(end_texts.contains(&"attempt 1 failed: exit(1)".to_string()));
        assert!(end_texts.contains(&"attempt 2 failed: exit(1)".to_string()));

        let started_texts = app
            .messages
            .iter()
            .filter(|message| message.kind == MessageKind::Started)
            .map(|message| message.text.clone())
            .collect::<Vec<_>>();
        assert!(started_texts.contains(&"retrying with gemini/gemini-2.5-pro".to_string()));
        assert!(started_texts.contains(&"retrying with codex/gpt-5".to_string()));
    });
}

#[test]
fn coder_finalize_succeeds_from_stable_advancing_finish_stamp() {
    with_temp_root(|| {
        let session_id = "coder-stable-advance";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "stable",
        );

        app.finalize_current_run(&run).expect("finalize coder");

        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Done);
        assert_eq!(finalized.error, None);
        assert_eq!(app.state.current_phase, Phase::ReviewRound(1));
    });
}

#[test]
fn coder_gate_reports_authoritative_failure_when_stamp_head_matches_base() {
    with_temp_root(|| {
        let session_id = "coder-stable-unchanged";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "base123",
            "stable",
        );

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        let reason = app
            .normalized_failure_reason(&run)
            .expect("normalized failure reason");
        assert_eq!(reason.as_deref(), Some("missing_coder_summary"));
    });
}

#[test]
fn coder_gate_fails_unverified_when_finish_stamp_missing_or_unstable() {
    with_temp_root(|| {
        let session_id = "coder-missing-stamp";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        let missing_reason = app
            .normalized_failure_reason(&run)
            .expect("missing normalized failure reason");
        let missing = missing_reason.expect("missing stamp should fail");
        assert!(missing.starts_with("failed_unverified"));
        assert!(missing.contains("missing finish stamp"));

        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "unstable",
        );
        let unstable_reason = app
            .normalized_failure_reason(&run)
            .expect("unstable normalized failure reason");
        let unstable = unstable_reason.expect("unstable stamp should fail");
        assert!(unstable.starts_with("failed_unverified"));
        assert!(unstable.contains("head_state=unstable"));
    });
}

#[test]
fn coder_gate_fails_unverified_when_finish_stamp_is_malformed() {
    with_temp_root(|| {
        let session_id = "coder-malformed-stamp";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let run_key = App::run_key_for("coder", Some(1), 1, 1);
        let stamp_path = session_dir
            .join("artifacts")
            .join("run-finish")
            .join(format!("{run_key}.toml"));
        std::fs::create_dir_all(stamp_path.parent().expect("stamp dir")).expect("stamp dir");
        std::fs::write(&stamp_path, "not = [valid").expect("write malformed stamp");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        let reason = app
            .normalized_failure_reason(&run)
            .expect("normalized failure reason");
        let reason = reason.expect("malformed stamp should fail");
        assert!(reason.starts_with("failed_unverified"));
        assert!(reason.contains("malformed finish stamp"));
    });
}

#[test]
fn coder_finalize_marks_missing_stamp_as_failed_unverified_with_hint() {
    with_temp_root(|| {
        let session_id = "coder-finalize-missing-stamp";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        app.finalize_current_run(&run).expect("finalize coder");

        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::FailedUnverified);
        assert!(
            finalized
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("run-finish")
        );
        let end = app
            .messages
            .iter()
            .find(|message| message.run_id == 1 && message.kind == MessageKind::End)
            .expect("end message");
        assert!(end.text.contains("attempt 1 unverified"));
        assert!(end.text.contains("missing finish stamp"));
    });
}

#[test]
fn coder_retry_exhaustion_enters_builder_recovery() {
    with_temp_root(|| {
        let session_id = "coder-retry-exhaustion";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.pending = vec![2, 3];
        state.builder.current_task = Some(1);
        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Claude, "claude-sonnet", 10, 1, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
        ];
        let harness = std::sync::Arc::new(std::sync::Mutex::new(TestLaunchHarness {
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
        }));
        app.test_launch_harness = Some(harness);

        app.launch_coder();
        for _ in 0..5 {
            if app.current_run_id.is_none() {
                break;
            }
            app.poll_agent_run();
        }

        assert!(app.current_run_id.is_none());
        assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
        assert_eq!(app.state.builder.current_task, None);
        assert_eq!(app.state.builder.pending, vec![2, 3]);
        let summary = app
            .state
            .builder
            .recovery_trigger_summary
            .clone()
            .expect("recovery trigger summary");
        assert!(summary.starts_with("retry exhausted (2 attempts)"));
        assert!(summary.contains("attempt 1: claude/claude-sonnet"));
        assert!(summary.contains("attempt 2: gemini/gemini-2.5-pro"));
    });
}

#[test]
fn coder_launch_records_modes_snapshot() {
    with_temp_root(|| {
        let session_id = "coder-launch-modes";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.modes.yolo = true;
        state.modes.cheap = true;

        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Claude, "claude-opus-4-7", 10, 1, 10),
            ranked_model(
                selection::VendorKind::Claude,
                "claude-sonnet-4-6",
                10,
                10,
                10,
            ),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_coder_with_model(None));

        let run = app
            .state
            .agent_runs
            .last()
            .expect("launch should create a run record");
        assert_eq!(
            run.modes,
            crate::state::LaunchModes {
                yolo: true,
                cheap: true,
                interactive: false,
            }
        );
        assert_eq!(run.model, "claude-sonnet-4-6");
        assert_eq!(run.effort, EffortLevel::Low);
        assert!(run.window_name.ends_with(":low"));
    });
}

#[test]
fn planning_launch_failure_surfaces_status_line_and_agent_error() {
    with_temp_root(|| {
        let session_id = "planning-launch-failure";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;

        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        std::fs::write(session_dir.join("artifacts/spec.md"), "# Spec\n").expect("write spec");

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
                    exit_code: 1,
                    artifact_contents: None,
                    launch_error: Some("spawn denied".to_string()),
                }]),
            },
        )));

        assert!(!app.launch_planning_with_model(None, true));

        let error = app
            .state
            .agent_error
            .as_deref()
            .expect("planning launch failure should set agent_error");
        assert!(error.contains("failed to launch planning"));
        assert!(error.contains("spawn denied"));

        let status = app.status_line.borrow().render().expect("status flash");
        assert!(status.to_string().contains("failed to launch planning"));

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("failed to launch planning"));
    });
}

#[test]
fn watcher_setup_uses_fast_synthetic_watcher_in_tests() {
    with_temp_root(|| {
        let session_id = "watcher-missing-live-summary";
        let state = SessionState::new(session_id.to_string());
        let mut app = idle_app(state);
        let artifacts_dir = session_state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        app.live_summary_path = Some(artifacts_dir.join("live_summary.txt"));

        app.setup_watcher().expect("watcher setup should succeed");

        assert!(app.live_summary_watcher.is_none());
        assert!(app.live_summary_change_events.is_some());
        assert!(app.status_line.borrow().render().is_none());
        assert!(
            !session_state::session_dir(session_id)
                .join("events.toml")
                .exists()
        );
    });
}

#[test]
fn watcher_setup_failure_surfaces_status_line_and_keeps_poll_fallback() {
    with_temp_root(|| {
        let session_id = "watcher-setup-failure";
        let state = SessionState::new(session_id.to_string());
        let mut app = idle_app(state);
        let blocked_parent = session_state::session_dir(session_id).join("not-a-directory");
        std::fs::create_dir_all(blocked_parent.parent().expect("session dir"))
            .expect("session dir");
        std::fs::write(&blocked_parent, "file").expect("blocked parent");
        app.live_summary_path = Some(blocked_parent.join("live_summary.txt"));

        app.setup_watcher().expect("watcher setup should fall back");

        assert!(app.live_summary_watcher.is_none());
        assert!(app.live_summary_change_events.is_none());

        let status = app.status_line.borrow().render().expect("status flash");
        let rendered = status.to_string();
        assert!(rendered.contains("watcher setup failed"));
        assert!(rendered.contains("falling back to poll"));

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("watcher setup failed"));
    });
}

#[test]
fn cheap_coder_fallback_logs_warning_when_budget_models_exhausted() {
    with_temp_root(|| {
        let session_id = "cheap-fallback-coder";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.modes.cheap = true;

        let mut cheap_model = ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet-4-6",
            10,
            1,
            10,
        );
        cheap_model.quota_percent = Some(0);
        let expensive = ranked_model(selection::VendorKind::Claude, "claude-opus-4-7", 10, 1, 10);

        let mut app = idle_app(state);
        app.models = vec![cheap_model, expensive];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_coder_with_model(None));
        let run = app.state.agent_runs.last().expect("run");
        assert_eq!(run.model, "claude-opus-4-7");
        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("cheap_fallback: phase=build reason=no_eligible_with_quota"));
    });
}

#[test]
fn cheap_toggle_persists_audits_flashes_and_preserves_running_snapshot() {
    with_temp_root(|| {
        let session_id = "cheap-toggle";
        let mut state = SessionState::new(session_id.to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Round 1 Coder] gpt-5".to_string(),
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
        state.save().expect("save session");
        let mut app = idle_app(state);

        // Toggle cheap via palette: `:` → type "cheap" → Enter
        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "cheap".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(app.state.modes.cheap);
        assert!(
            !app.state.agent_runs[0].modes.cheap,
            "already-running snapshots must not be rewritten"
        );
        assert!(SessionState::load(session_id).expect("reload").modes.cheap);
        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("mode_toggled: mode=cheap value=true source=palette"));
        let status = app.status_line.borrow().render().expect("status flash");
        assert!(status.to_string().contains("cheap: ON"));
    });
}

#[test]
fn palette_invocation_is_audited_with_command_and_args() {
    with_temp_root(|| {
        let session_id = "palette-invoked";
        let state = SessionState::new(session_id.to_string());
        state.save().expect("save session");
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "cheap on".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("palette_invoked: command=cheap args=on"));
        assert!(events.contains("mode_toggled: mode=cheap value=true source=palette"));
    });
}

#[test]
fn brainstorm_failure_auto_retries_with_next_model() {
    with_temp_root(|| {
        let session_id = "brainstorm-retry";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("idea".to_string());
        let run = RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        };
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Claude, "claude-sonnet", 1, 1, 1),
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 1, 1),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        write_finish_stamp_for_run(&app, &run, 1, "");

        app.finalize_current_run(&run)
            .expect("finalize brainstorm failure");
        assert_eq!(
            app.failed_models
                .get(&("brainstorm".to_string(), None, 1))
                .map(|set| set.len()),
            Some(1)
        );
        assert_eq!(app.state.agent_runs.len(), 2);
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
        assert_eq!(app.state.agent_runs[1].status, RunStatus::Running);
        assert_eq!(app.state.agent_runs[1].stage, "brainstorm");
    });
}
