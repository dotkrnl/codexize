use super::*;

#[test]
fn normalize_failure_reason_reports_exit_signal_and_artifact_errors() {
    with_temp_root(|| {
        let session_id = "normalize-failure-reason";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let state = SessionState::new(session_id.to_string());
        let mut app = mk_app(state);
        let run = RunRecord {
            id: 9,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };

        write_finish_stamp_for_run(&app, &run, 1, "");
        assert_eq!(
            app.normalized_failure_reason(&run).expect("exit reason"),
            Some("exit(1)".to_string())
        );

        let signal_stamp_path = app.finish_stamp_path_for(&run);
        crate::runner::write_finish_stamp(
            &signal_stamp_path,
            &crate::runner::FinishStamp {
                finished_at: chrono::Utc::now().to_rfc3339(),
                exit_code: 143,
                head_before: "before".to_string(),
                head_after: "after".to_string(),
                head_state: "stable".to_string(),
                signal_received: String::new(),
                working_tree_clean: true,
            },
        )
        .expect("write signal stamp");
        write_finish_stamp_for_run(&app, &run, 143, "");
        assert_eq!(
            app.normalized_failure_reason(&run).expect("signal reason"),
            Some("killed(15) [agent exited 143]".to_string())
        );
        app.state
            .log_event("agent_stopped_by_user: run_id=9")
            .expect("log user stop marker");
        assert_eq!(
            app.normalized_failure_reason(&run)
                .expect("operator-killed signal reason"),
            Some("Operator Killed".to_string())
        );

        let hup_run = RunRecord {
            id: 10,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 2,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Planning 2]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        crate::runner::write_finish_stamp(
            &app.finish_stamp_path_for(&hup_run),
            &crate::runner::FinishStamp {
                finished_at: chrono::Utc::now().to_rfc3339(),
                exit_code: 129,
                head_before: "before".to_string(),
                head_after: "after".to_string(),
                head_state: "stable".to_string(),
                signal_received: "HUP".to_string(),
                working_tree_clean: true,
            },
        )
        .expect("write hup stamp");
        assert_eq!(
            app.normalized_failure_reason(&hup_run)
                .expect("hup signal reason"),
            Some("killed(1) [wrapper trapped HUP]".to_string())
        );

        let self_exit_run = RunRecord {
            id: 11,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 3,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Planning 3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        crate::runner::write_finish_stamp(
            &app.finish_stamp_path_for(&self_exit_run),
            &crate::runner::FinishStamp {
                finished_at: chrono::Utc::now().to_rfc3339(),
                exit_code: 129,
                head_before: "before".to_string(),
                head_after: "after".to_string(),
                head_state: "stable".to_string(),
                signal_received: String::new(),
                working_tree_clean: true,
            },
        )
        .expect("write self-exit stamp");
        assert_eq!(
            app.normalized_failure_reason(&self_exit_run)
                .expect("self-exit reason"),
            Some("killed(1) [agent exited 129]".to_string())
        );
        let events_text =
            std::fs::read_to_string(session_dir.join("events.toml")).expect("read events log");
        assert!(
                events_text.contains(
                    "run 11 (planning) exited 129: signal_received= (agent CLI exited 129 on its own; wrapper trapped no signal)"
                ),
                "self-exit diagnostic must be logged explicitly: {events_text}"
            );

        write_finish_stamp_for_run(&app, &run, 0, "");
        assert_eq!(
            app.normalized_failure_reason(&run)
                .expect("missing artifact"),
            Some("artifact_missing".to_string())
        );

        std::fs::write(session_dir.join("artifacts").join("plan.md"), "")
            .expect("write empty plan");
        assert_eq!(
            app.normalized_failure_reason(&run).expect("empty artifact"),
            Some("artifact_missing".to_string())
        );

        let brainstorm = RunRecord {
            stage: "brainstorm".to_string(),
            window_name: "[Brainstorm]".to_string(),
            ..run.clone()
        };
        write_finish_stamp_for_run(&app, &brainstorm, 0, "");
        std::fs::write(session_dir.join("artifacts").join("spec.md"), "")
            .expect("write empty spec");
        assert_eq!(
            app.normalized_failure_reason(&brainstorm)
                .expect("empty spec"),
            Some("artifact_missing".to_string())
        );

        let sharding = RunRecord {
            stage: "sharding".to_string(),
            window_name: "[Sharding]".to_string(),
            ..run.clone()
        };
        write_finish_stamp_for_run(&app, &sharding, 0, "");
        std::fs::write(
            session_dir.join("artifacts").join("tasks.toml"),
            "not valid toml = [",
        )
        .expect("write invalid tasks");
        assert!(
            app.normalized_failure_reason(&sharding)
                .expect("invalid tasks")
                .expect("error text")
                .starts_with("artifact_invalid: ")
        );
    });
}

#[test]
fn operator_stopped_run_finalizes_without_agent_error() {
    with_temp_root(|| {
        let session_id = "operator-stop-no-modal";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let run = make_brainstorm_run(7);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.pending_termination = Some(PendingTermination::new_stop_only(run.id));

        app.state
            .log_event(format!("agent_stopped_by_user: run_id={}", run.id))
            .expect("log stop marker");
        write_finish_stamp_for_run(&app, &run, 143, "");

        app.poll_agent_run();

        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|candidate| candidate.id == run.id)
            .expect("finalized run");
        assert_eq!(finalized.status, RunStatus::Failed);
        assert_eq!(finalized.error.as_deref(), Some("Operator Killed"));
        assert!(app.state.agent_error.is_none());
        assert_eq!(app.active_modal(), None);
    });
}

#[test]
fn normalize_failure_reason_artifact_present_still_fails_on_head_advance() {
    with_temp_root(|| {
        let session_id = "normalize-failure-reason-guard";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let state = SessionState::new(session_id.to_string());
        let mut app = mk_app(state);
        let run = RunRecord {
            id: 1,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };

        // Valid plan artifact so artifact_reason is None.
        std::fs::write(session_dir.join("artifacts").join("plan.md"), "# Plan\n")
            .expect("write plan");
        write_finish_stamp_for_run(&app, &run, 0, "");

        // Write a guard snapshot whose HEAD differs from real HEAD so
        // verify_non_coder will return forbidden_head_advance.
        let guard_dir = session_dir.join(".guards").join("planning-stage-r1-a1");
        std::fs::create_dir_all(&guard_dir).expect("guard dir");
        std::fs::write(
                guard_dir.join("snapshot.toml"),
                "head = \"0000000000000000000000000000000000000000\"\ngit_status = \"\"\n\n[control_files]\n",
            )
            .expect("write snapshot");

        let reason = app
            .normalized_failure_reason(&run)
            .expect("normalized")
            .expect("hard error expected");
        assert_eq!(reason, "forbidden_head_advance");
    });
}

#[test]
fn window_disappearance_enters_drain_state_before_finalize() {
    with_temp_root(|| {
        let session_id = "planning-drain-before-finalize";
        let session_dir = session_state::session_dir(session_id);
        let artifacts_dir = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        std::fs::write(artifacts_dir.join("plan.md"), "# Plan\n").expect("plan artifact");

        let run = make_planning_run(1, 1);
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            1,
            10,
            10,
        )];

        let _ = std::fs::remove_file(app.finish_stamp_path_for(&run));

        app.poll_agent_run();

        let persisted = app
            .state
            .agent_runs
            .iter()
            .find(|candidate| candidate.id == run.id)
            .expect("run");
        assert_eq!(persisted.status, RunStatus::Running);
        assert_eq!(app.current_run_id, Some(run.id));
        assert!(app.pending_drain_deadline.is_some());
    });
}

#[test]
fn same_key_retry_waits_for_stamp_or_timeout_after_live_summary_absent() {
    with_temp_root(|| {
        let session_id = "planning-drain-timeout";
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
        let stamp_path = app.finish_stamp_path_for(&first);
        let _ = std::fs::remove_file(&stamp_path);
        let _ = std::fs::remove_file(app.live_summary_path_for(&first));

        app.poll_agent_run();
        assert_eq!(app.current_run_id, Some(first.id));
        let still_first = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == first.id)
            .expect("first run after barrier");
        assert_eq!(still_first.status, RunStatus::Running);

        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
        app.poll_agent_run();

        let first_done = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == first.id)
            .expect("first finalized");
        assert_eq!(first_done.status, RunStatus::Failed);
        let second = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.stage == "planning" && run.attempt == 2)
            .expect("retry attempt 2 launched");
        assert_eq!(second.status, RunStatus::Running);
        assert_eq!(app.current_run_id, Some(second.id));
    });
}

#[test]
fn failed_unverified_coder_does_not_auto_retry() {
    with_temp_root(|| {
        let session_id = "coder-unverified-no-retry";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
        ];

        app.finalize_current_run(&run).expect("finalize coder");

        assert_eq!(app.state.agent_runs.len(), 1);
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|candidate| candidate.id == run.id)
            .expect("finalized run");
        assert_eq!(finalized.status, RunStatus::FailedUnverified);
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("failed_unverified:"),
            "failed_unverified should block auto-retry and surface as agent_error"
        );
    });
}

#[test]
fn guard_warnings_emit_only_after_drain_barrier_passes() {
    with_temp_root(|| {
        let session_id = "guard-after-drain";
        let session_dir = session_state::session_dir(session_id);
        let artifacts_dir = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        std::fs::write(artifacts_dir.join("plan.md"), "# Plan\n").expect("plan artifact");

        let run = make_planning_run(1, 1);
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        let _ = std::fs::remove_file(app.finish_stamp_path_for(&run));

        let guard_dir = session_dir.join(".guards").join("planning-stage-r1-a1");
        std::fs::create_dir_all(&guard_dir).expect("guard dir");
        std::fs::write(
            guard_dir.join("snapshot.toml"),
            "head = \"\"\ngit_status = \"dirty\"\nmode = \"auto_reset\"\n\n[control_files]\n",
        )
        .expect("guard snapshot");

        app.poll_agent_run();

        assert!(
            !app.messages.iter().any(|message| {
                message.run_id == run.id
                    && message.kind == MessageKind::SummaryWarn
                    && message
                        .text
                        .contains("working tree was dirty before agent launch")
            }),
            "guard diagnostics should not emit before drain barrier releases finalize"
        );

        let run_key = App::run_key_for("planning", None, 1, 1);
        write_finish_stamp(&session_dir, &run_key, "head123", "stable");
        app.poll_agent_run();

        assert!(
            app.messages.iter().any(|message| {
                message.run_id == run.id
                    && message.kind == MessageKind::SummaryWarn
                    && message
                        .text
                        .contains("working tree was dirty before agent launch")
            }),
            "guard diagnostics should emit after barrier passes"
        );
    });
}

#[test]
fn rapid_retry_cycles_do_not_attribute_stale_live_summary_to_next_attempt() {
    with_temp_root(|| {
        let session_id = "rapid-live-summary-isolation";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut app = idle_app(state);

        for attempt in 1..=50 {
            let previous = make_planning_run((attempt * 2 - 1) as u64, attempt);
            let current = make_planning_run((attempt * 2) as u64, attempt + 1);
            app.state.agent_runs.push(previous.clone());
            app.state.agent_runs.push(current.clone());
            app.current_run_id = Some(current.id);
            app.run_launched = true;
            app.live_summary_path = Some(app.live_summary_path_for(&current));
            app.live_summary_cached_text.clear();
            app.live_summary_cached_mtime = Some(std::time::SystemTime::UNIX_EPOCH);

            let stale_path = app.live_summary_path_for(&previous);
            std::fs::create_dir_all(stale_path.parent().expect("summary dir"))
                .expect("summary dir");
            std::fs::write(
                &stale_path,
                format!("stale attempt {attempt} from previous run\n"),
            )
            .expect("write stale summary");

            app.poll_live_summary_fallback();

            assert!(
                !app.messages.iter().any(|message| {
                    message.run_id == current.id
                        && message.kind == MessageKind::Brief
                        && message.text.contains("stale attempt")
                }),
                "attempt {} stale summary was attributed to successor run",
                attempt
            );

            std::fs::remove_file(stale_path).expect("remove stale summary");
        }
    });
}

#[test]
fn unstable_coder_stamp_finalizes_failed_unverified_without_retry() {
    with_temp_root(|| {
        let session_id = "coder-unstable-stamp-no-retry";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
        ];

        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "flapping-head",
            "unstable",
        );
        let _ = std::fs::remove_file(app.live_summary_path_for(&run));

        app.poll_agent_run();

        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|candidate| candidate.id == run.id)
            .expect("finalized run");
        assert_eq!(finalized.status, RunStatus::FailedUnverified);
        assert!(
            finalized
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("head_state=unstable")
        );
        assert_eq!(
            app.state
                .agent_runs
                .iter()
                .filter(|candidate| candidate.stage == "coder")
                .count(),
            1,
            "failed_unverified coder runs must not launch a retry"
        );
    });
}

#[test]
fn recovery_sharding_retry_uses_recovery_launcher() {
    with_temp_root(|| {
        let session_id = "recovery-sharding-retry";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# spec\n").unwrap();
        std::fs::write(session_dir.join("artifacts").join("plan.md"), "# plan\n").unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecoverySharding(6);
        state.builder.done = vec![1, 2];
        state.builder.pending = vec![3];
        state.builder.iteration = 6;

        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Claude, "claude-opus-4-7", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
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

        let failed = RunRecord {
            id: 25,
            stage: "sharding".to_string(),
            task_id: None,
            round: 6,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Recovery Sharding] opus-4-7".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("recovery_sharding_failed: tasks.toml missing".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        app.state.agent_runs.push(failed.clone());

        let handled = app.maybe_auto_retry(&failed);
        assert!(handled, "auto-retry must fire for recovery sharding");

        // Newly launched run must be a recovery-sharding run at round=6,
        // not a fresh round=1 sharding run from the original launcher.
        let new_run = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id != 25 && r.stage == "sharding")
            .expect("retry should create a new sharding run");
        assert_eq!(
            new_run.round, 6,
            "retry must keep the recovery round, not reset to round 1"
        );
        assert!(
            new_run.window_name.starts_with("[Recovery Sharding]"),
            "retry must use the recovery-sharding run label, got: {}",
            new_run.window_name
        );
        assert_eq!(app.state.current_phase, Phase::BuilderRecoverySharding(6));
    });
}
