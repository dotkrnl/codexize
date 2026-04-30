// tests_launch.rs
use super::*;
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    selection::{self, ranking::build_version_index},
    state::{
        self as session_state, MessageKind, Phase, PipelineItem, RunRecord, RunStatus, SessionState,
    },
};

#[test]
fn brainstorm_selection_uses_idea_task_kind() {
    let models = vec![
        sample_model("idea-first", 1, 2),
        sample_model("build-first", 2, 1),
    ];

    let versions = build_version_index(&models);
    let chosen =
        App::select_brainstorm_model(&models, &versions).expect("expected brainstorm model");

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
fn watcher_setup_allows_missing_live_summary_file_under_existing_artifacts_dir() {
    with_temp_root(|| {
        let session_id = "watcher-missing-live-summary";
        let state = SessionState::new(session_id.to_string());
        let mut app = idle_app(state);
        let artifacts_dir = session_state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        app.live_summary_path = Some(artifacts_dir.join("live_summary.txt"));

        app.setup_watcher().expect("watcher setup should succeed");

        assert!(app.live_summary_watcher.is_some());
        assert!(app.live_summary_change_rx.is_some());
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
        assert!(app.live_summary_change_rx.is_none());

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

#[test]
fn brainstorm_finalization_overlength_nothing_to_do_enters_skip_pending() {
    with_temp_root(|| {
        let session_id = "brainstorm-skip-overlength";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;

        let run = RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
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
        state.agent_runs.push(run.clone());

        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("mk artifacts dir");

        let rationale = "x".repeat(520);
        let proposal_toml = format!(
            "proposed = true\nstatus = \"nothing_to_do\"\nrationale = \"{}\"\n",
            rationale
        );
        std::fs::write(artifacts.join("skip_proposal.toml"), proposal_toml)
            .expect("write skip proposal");

        let mut app = idle_app(state);
        app.complete_run_finalization(&run, None)
            .expect("finalization should succeed");

        assert_eq!(app.state.current_phase, Phase::SkipToImplPending);
        assert_eq!(
            app.state.skip_to_impl_kind,
            Some(crate::artifacts::SkipProposalStatus::NothingToDo)
        );
        let stored_rationale = app
            .state
            .skip_to_impl_rationale
            .expect("rationale should be set");
        assert_eq!(stored_rationale.chars().count(), 500);
    });
}

// ── Recovery circuit-breaker and queue validation tests ──────────────────

#[test]
fn launch_recovery_uses_interactive_prompt_for_human_blocked() {
    use crate::state::PipelineItemStatus;
    with_temp_root(|| {
        let session_id = "recovery-interactive-launch";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        state.builder.recovery_trigger_task_id = Some(1);
        state.builder.recovery_trigger_summary = Some("needs human judgment".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: Some("Human-blocked recovery".to_string()),
            mode: None,
            trigger: Some("human_blocked".to_string()),
            interactive: Some(true),
        });

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
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        let ok = app.launch_recovery_with_model(None);
        assert!(ok, "launch_recovery_with_model must succeed");

        let prompt_path = session_dir.join("prompts").join("recovery-r1.md");
        let prompt = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            prompt.contains("INTERACTIVE"),
            "human_blocked recovery prompt file must be INTERACTIVE"
        );
        assert!(
            !prompt.contains("NON-INTERACTIVE"),
            "human_blocked recovery prompt file must not be NON-INTERACTIVE"
        );
    });
}

#[test]
fn launch_recovery_uses_noninteractive_prompt_for_agent_pivot() {
    use crate::state::PipelineItemStatus;
    with_temp_root(|| {
        let session_id = "recovery-noninteractive-launch";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        state.builder.recovery_trigger_task_id = Some(1);
        state.builder.recovery_trigger_summary = Some("plan is wrong".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(2),
            status: PipelineItemStatus::Running,
            title: Some("Agent pivot recovery".to_string()),
            mode: None,
            trigger: Some("agent_pivot".to_string()),
            interactive: Some(false),
        });

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
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        let ok = app.launch_recovery_with_model(None);
        assert!(ok, "launch_recovery_with_model must succeed");

        let prompt_path = session_dir.join("prompts").join("recovery-r2.md");
        let prompt = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            prompt.contains("NON-INTERACTIVE"),
            "agent_pivot recovery prompt file must be NON-INTERACTIVE"
        );
    });
}

// ---------- pending guard decision tests ----------

#[test]
fn coder_gate_accepts_done_summary_without_head_advance() {
    with_temp_root(|| {
        let session_id = "coder-summary-done";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "base123",
            "stable",
        );
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            r#"status = "done"
summary = "Already complete"
"#,
        )
        .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        assert_eq!(app.coder_gate_reason(&run, &round_dir), None);
    });
}

#[test]
fn coder_gate_retries_partial_summary_even_after_head_advances() {
    with_temp_root(|| {
        let session_id = "coder-summary-partial";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "stable",
        );
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            r#"status = "partial"
summary = "Still working"
"#,
        )
        .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        assert_eq!(
            app.coder_gate_reason(&run, &round_dir).as_deref(),
            Some("coder_partial")
        );
    });
}

#[test]
fn coder_gate_rejects_invalid_summary_even_after_head_advances() {
    with_temp_root(|| {
        let session_id = "coder-summary-invalid";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "stable",
        );
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            r#"status = "done"
summary = "   "
"#,
        )
        .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        assert_eq!(
            app.coder_gate_reason(&run, &round_dir).as_deref(),
            Some("invalid_coder_summary")
        );
    });
}

#[test]
fn coder_gate_rejects_dirty_working_tree_finish_stamp() {
    with_temp_root(|| {
        let session_id = "coder-dirty-finish-stamp";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        let stamp = crate::runner::FinishStamp {
            finished_at: chrono::Utc::now().to_rfc3339(),
            exit_code: 0,
            head_before: "base123".to_string(),
            head_after: "head456".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
            working_tree_clean: false,
        };
        let stamp_path = session_dir
            .join("artifacts")
            .join("run-finish")
            .join(format!("{}.toml", App::run_key_for("coder", Some(1), 1, 1)));
        crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write finish stamp");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        let reason = app.coder_gate_reason(&run, &round_dir).unwrap();
        assert!(reason.contains("working tree not clean on exit"));
    });
}

#[test]
fn reviewer_prompt_always_scopes_base_to_head() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("reviewer-prompt-base-head");
        let task_file = session_dir.join("rounds/001/task.toml");
        let scope_file = session_dir.join("rounds/001/review_scope.toml");
        let summary_file = session_dir.join("rounds/001/coder_summary.toml");
        let review_file = session_dir.join("rounds/001/review.toml");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(task_file.parent().unwrap()).unwrap();

        let prompt = reviewer_prompt(ReviewerPromptInputs {
            session_dir: &session_dir,
            task_id: 1,
            round: 2,
            task_file: &task_file,
            review_scope_file: &scope_file,
            coder_summary_file: Some(&summary_file),
            review_file: &review_file,
            live_summary_path: &live_summary,
        });

        assert!(!prompt.contains("DIRTY WORKING TREE"));
        assert!(!prompt.contains("git diff HEAD"));
        assert!(!prompt.contains("git ls-files --others --exclude-standard"));
        assert!(prompt.contains("review only `base..HEAD`"));
        assert!(prompt.contains("Coder summary:"));
        assert!(prompt.contains("Coder rebuttal (round 2):"));
    });
}

#[test]
fn brainstorm_prompts_require_authoritative_user_requirements() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("brainstorm-authoritative-section");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        for yolo in [false, true] {
            let prompt = brainstorm_prompt(
                "add retries unless disabled",
                &spec_path.display().to_string(),
                &summary_path.display().to_string(),
                &live_summary.display().to_string(),
                yolo,
            );

            assert!(prompt.contains("## User-stated requirements (authoritative)"));
            assert!(
                prompt.contains("Quote each user-stated decision from the Idea above verbatim")
            );
            assert!(prompt.contains("Use the user's own wording, not a paraphrase."));
            assert!(prompt.contains("Never silently reinterpret."));
            if yolo {
                assert!(prompt.contains("pick the narrowest reasonable reading"));
                assert!(prompt.contains("recording the choice under\n`## Assumptions`"));
            } else {
                assert!(prompt.contains("statement is ambiguous, ask the operator."));
                assert!(prompt.contains(
                    "If two user statements conflict with\neach other, ask the operator."
                ));
            }
        }
    });
}

#[test]
fn coder_prompt_requires_clean_exit_and_new_summary_schema() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("coder-clean-exit-prompt");
        let round_dir = session_dir.join("rounds/001");
        let task_file = round_dir.join("task.toml");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(&round_dir).unwrap();

        let prompt = coder_prompt(&session_dir, 1, 1, &task_file, &live_summary, false, &[]);

        assert!(prompt.contains("Working tree must be clean on exit."));
        assert!(prompt.contains("git status --porcelain` MUST be empty when you stop"));
        assert!(prompt.contains("tree dirty is a hard failure"));
        assert!(!prompt.contains("dirty_before"));
        assert!(!prompt.contains("dirty_after"));
        assert!(prompt.contains("independently verifies the working tree is clean"));
    });
}

#[test]
fn planning_prompt_flags_ai_written_reviews_for_triage() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("planning-prompt-ai-reviews");
        let spec_path = session_dir.join("artifacts/spec.md");
        let plan_path = session_dir.join("artifacts/plan.md");
        let review_path = session_dir.join("artifacts/spec-review-1.md");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        std::fs::write(&review_path, "review").unwrap();

        let prompt = planning_prompt(&spec_path, &[review_path], &plan_path, &live_summary, false);

        assert!(prompt.contains("written by AI"));
        assert!(prompt.contains("be skeptical"));
        assert!(prompt.contains("genuinely improves the spec or plan"));
        assert!(prompt.contains("reject the rest with a brief reason"));
    });
}

#[test]
fn coder_prompt_tells_resume_rounds_to_rebut_unhelpful_ai_feedback() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("coder-prompt-ai-feedback");
        let round_dir = session_dir.join("rounds/001");
        let task_file = round_dir.join("task.toml");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(&round_dir).unwrap();
        std::fs::write(round_dir.join("review.toml"), "feedback").unwrap();

        let prompt = coder_prompt(&session_dir, 1, 2, &task_file, &live_summary, true, &[]);

        assert!(prompt.contains("Previous reviewer feedback (round 1):"));
        assert!(prompt.contains("Reviewer feedback comes from an AI agent."));
        assert!(prompt.contains(
                "Evaluate each item critically — address what improves the code, rebut the rest in coder_summary.toml."
            ));
    });
}

// Modal tests

#[test]
fn brainstorm_error_e_transitions_to_idea_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_error = Some("failed".to_string());
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char('e')));
        assert_eq!(app.state.current_phase, Phase::IdeaInput);
    });
}
