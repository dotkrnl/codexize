use super::*;

#[test]
fn final_validation_auto_launches_via_maybe_auto_launch() {
    with_temp_root(|| {
        let session_id = "final-validation-auto";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").expect("spec");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(1);
        state.idea_text = Some("idea".to_string());
        state.selected_model = Some("gpt-5".to_string());

        let mut app = idle_app(state);
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
                    artifact_contents: Some(
                        "status = \"goal_met\"\nsummary = \"ok\"\nfindings = []\n".to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        app.maybe_auto_launch();
        let run = app
            .state
            .agent_runs
            .last()
            .expect("auto-launch must record a run");
        assert_eq!(run.stage, "final-validation");
        assert_eq!(run.round, 1);
    });
}

#[test]
fn picker_created_brainstorm_auto_launches_after_first_frame() {
    with_temp_root(|| {
        let session_id = "picker-created-auto-launch";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("Launch after first frame".to_string());
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

        app.on_frame_drawn();
        app.maybe_auto_launch();

        let run = app
            .state
            .agent_runs
            .last()
            .expect("picker-created startup should launch after the first frame");
        assert_eq!(run.stage, "brainstorm");
        assert_eq!(run.round, 1);
    });
}

#[test]
fn default_startup_brainstorm_auto_launch_is_not_gated() {
    with_temp_root(|| {
        let session_id = "default-auto-launch";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("Resume should not wait".to_string());
        let mut app = idle_app(state);
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

        let run = app
            .state
            .agent_runs
            .last()
            .expect("default startup should auto-launch immediately");
        assert_eq!(run.stage, "brainstorm");
    });
}

#[test]
fn final_validation_launch_without_models_records_agent_error() {
    with_temp_root(|| {
        let session_id = "final-validation-no-models";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(1);
        let mut app = idle_app(state);

        assert!(!app.launch_final_validation_with_model(None));
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .contains("model list not yet loaded")
        );
        assert!(app.state.agent_runs.is_empty());
    });
}

#[test]
fn simplifier_launch_reuses_most_recent_coder_model_for_round() {
    with_temp_root(|| {
        let session_id = "simplifier-coder-model";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        // Simplifier needs review_scope.toml to exist; round entry writes it
        // by Task 3 (round-entry hook), so seed it explicitly here.
        write_review_scope(&round_dir, "base-simp");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        // The most recent coder run for round 1 is attempt 2 with claude.
        // The first attempt (codex/gpt-5) must NOT be picked.
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 2,
            model: "claude-sonnet-4-6".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 10, 10),
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
                    artifact_contents: Some(
                        "status = \"no_changes\"\nsummary = \"diff is tight\"\n".to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_simplifier_with_model(None));

        let run = app
            .state
            .agent_runs
            .iter()
            .rev()
            .find(|run| run.stage == "simplifier")
            .expect("simplifier run recorded");
        assert_eq!(run.round, 1);
        assert_eq!(run.task_id, None);
        assert_eq!(run.attempt, 1);
        assert_eq!(run.model, "claude-sonnet-4-6");
        assert_eq!(run.vendor, "claude");
        assert!(
            run.window_name.starts_with("[Simplifier] "),
            "window label must start with `[Simplifier] `, got {}",
            run.window_name
        );
        // Required artifact must land where finalization looks for it.
        let simplification_path = round_dir.join("simplification.toml");
        assert!(
            simplification_path.exists(),
            "harness should have written simplification.toml"
        );
        // Live summary path follows the standard per-run convention.
        let live_summary_path = session_dir.join("artifacts").join(format!(
            "live_summary.{}.txt",
            App::run_key_for("simplifier", None, 1, 1)
        ));
        let prompt_path = session_dir.join("prompts").join("simplifier-r1.md");
        let prompt = std::fs::read_to_string(&prompt_path).expect("prompt file");
        assert!(prompt.contains(&simplification_path.display().to_string()));
        assert!(prompt.contains(&live_summary_path.display().to_string()));
        assert!(prompt.contains(&round_dir.join("review_scope.toml").display().to_string()));
    });
}

#[test]
fn simplifier_picks_chronologically_latest_coder_run_across_tasks() {
    // Multi-task rounds expose the "highest attempt is not newest run" trap:
    // task 1 attempt 2 has a higher attempt counter than task 2 attempt 1,
    // but task 2 ran later in wall time and reflects what the round most
    // recently settled on. The simplifier must follow run recency (by id),
    // not the attempt number, so an `attempt`-keyed selector would regress.
    with_temp_root(|| {
        let session_id = "simplifier-mixed-task-recency";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        write_review_scope(&round_dir, "base-mixed");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        // Task 1 retried once (attempts 1 then 2 on claude). Task 2 then ran
        // its first attempt on codex, which is the chronologically most
        // recent coder run for round 1 even though its attempt counter is
        // lower than task 1's second try.
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet-4-6".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder T1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 2,
            model: "claude-sonnet-4-6".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder T1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 3,
            stage: "coder".to_string(),
            task_id: Some(2),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Round 1 Coder T2]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 10, 10),
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
                    artifact_contents: Some(
                        "status = \"no_changes\"\nsummary = \"diff is tight\"\n".to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_simplifier_with_model(None));

        let run = app
            .state
            .agent_runs
            .iter()
            .rev()
            .find(|run| run.stage == "simplifier")
            .expect("simplifier run recorded");
        assert_eq!(
            run.model, "gpt-5",
            "simplifier must follow the chronologically latest coder run \
             (task 2 attempt 1, id 3), not the highest attempt number \
             (task 1 attempt 2, id 2)"
        );
        assert_eq!(run.vendor, "codex");
    });
}

#[test]
fn simplifier_retry_reuses_existing_simplifier_run_model_over_coder() {
    with_temp_root(|| {
        let session_id = "simplifier-retry-reuse";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("002");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        write_review_scope(&round_dir, "base-r2");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(2);
        // A coder ran on this round, but the simplifier already locked in a
        // different model on its first attempt; retries must keep that model.
        state.agent_runs.push(RunRecord {
            id: 7,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 2,
            attempt: 1,
            model: "claude-sonnet-4-6".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 2 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 8,
            stage: "simplifier".to_string(),
            task_id: None,
            round: 2,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Simplifier]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 10, 10),
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
                    artifact_contents: Some(
                        "status = \"no_changes\"\nsummary = \"clean diff\"\n".to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_simplifier_with_model(None));

        let run = app
            .state
            .agent_runs
            .iter()
            .rev()
            .find(|run| run.stage == "simplifier" && run.attempt == 2)
            .expect("simplifier retry run recorded");
        assert_eq!(
            run.model, "gpt-5",
            "simplifier retry must reuse the prior simplifier model, not the coder's"
        );
        assert_eq!(run.vendor, "codex");
    });
}

#[test]
fn simplifier_refuses_to_launch_without_review_scope() {
    with_temp_root(|| {
        let session_id = "simplifier-missing-scope";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        // Deliberately do NOT write review_scope.toml.

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-opus-4-7",
            10,
            1,
            10,
        )];
        // No harness queued — if the launcher reaches the harness layer the
        // expect-on-pop will panic, signalling the missing scope guard
        // failed to short-circuit.
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::new(),
            },
        )));

        assert!(!app.launch_simplifier_with_model(None));
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .contains("invalid review scope"),
            "missing review scope must surface as an explicit launcher error: {:?}",
            app.state.agent_error
        );
        assert!(app.state.agent_runs.is_empty());
    });
}

#[test]
fn simplifier_auto_launches_via_maybe_auto_launch() {
    with_temp_root(|| {
        let session_id = "simplifier-auto-launch";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        write_review_scope(&round_dir, "base-auto");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        // Provide a prior coder run so the simplifier model resolves through
        // round_stage_model (Q5/b precedence) rather than falling through to
        // the primary picker, which test fixtures do not feed real ipbr
        // scores into.
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            10,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(
                        "status = \"no_changes\"\nsummary = \"clean\"\n".to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        app.maybe_auto_launch();
        let run = app
            .state
            .agent_runs
            .iter()
            .rev()
            .find(|run| run.stage == "simplifier")
            .expect("auto-launch must record a simplifier run");
        assert_eq!(run.round, 1);
    });
}

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
