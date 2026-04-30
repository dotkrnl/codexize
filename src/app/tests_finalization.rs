// tests_finalization.rs
use super::*;
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    selection,
    state::{
        self as session_state, MessageKind, Phase, PipelineItem, PipelineItemStatus, RunRecord,
        RunStatus, SessionState,
    },
};

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
            .log_event("agent_killed_by_user: run_id=9")
            .expect("log user kill marker");
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

#[test]
fn recovery_retry_exhaustion_falls_back_to_blocked() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-retry-cap".to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        state.builder.recovery_trigger_task_id = Some(7);
        state.builder.recovery_prev_max_task_id = Some(9);
        state.builder.recovery_prev_task_ids = vec![7, 8, 9];
        state.builder.recovery_trigger_summary = Some("stale trigger".to_string());
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet",
            1,
            10,
            10,
        )];
        let failed = RunRecord {
            id: 21,
            stage: "recovery".to_string(),
            task_id: None,
            round: 2,
            attempt: 3,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Recovery]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("artifact_invalid: x".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        let handled = app.maybe_auto_retry(&failed);
        assert!(handled);
        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("builder recovery retry exhausted")
        );
        assert_eq!(app.state.builder.recovery_trigger_task_id, Some(7));
        assert_eq!(app.state.builder.recovery_prev_max_task_id, Some(9));
        assert_eq!(app.state.builder.recovery_prev_task_ids, vec![7, 8, 9]);
        assert_eq!(
            app.state.builder.recovery_trigger_summary.as_deref(),
            Some("stale trigger")
        );
    });
}

#[test]
fn failed_recovery_entry_clears_recovery_context() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-entry-fail".to_string());
        state.current_phase = Phase::IdeaInput;
        state.builder.current_task = Some(3);
        let mut app = idle_app(state);

        let entered = app.enter_builder_recovery(
            1,
            Some(3),
            Some("cannot enter from idea".to_string()),
            "agent_pivot",
        );

        assert!(entered);
        assert!(app.state.agent_error.is_some());
        assert_eq!(app.state.builder.recovery_trigger_task_id, None);
        assert_eq!(app.state.builder.recovery_prev_max_task_id, None);
        assert!(app.state.builder.recovery_prev_task_ids.is_empty());
        assert_eq!(app.state.builder.recovery_trigger_summary, None);
    });
}

#[test]
fn recovery_requires_parseable_recovery_artifact() {
    with_temp_root(|| {
        let session_id = "recovery-invalid-artifact";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").expect("spec");
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("plan");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 2
title = "Recovered"
description = "valid"
test = "cargo test"
estimated_tokens = 10
"#,
        )
        .expect("tasks");
        std::fs::write(round_dir.join("recovery.toml"), "[[[broken").expect("recovery");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "recovery".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Recovery]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let mut app = idle_app(state);
        let run = app.state.agent_runs[0].clone();
        let reason = app
            .normalized_failure_reason(&run)
            .expect("normalized")
            .expect("failure reason");

        assert!(reason.starts_with("artifact_invalid:"), "{reason}");
    });
}

#[test]
fn recovery_status_revise_can_escalate_next_retry_to_human_blocked() {
    with_temp_root(|| {
        let session_id = "recovery-revise-human-blocked";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("007");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").expect("spec");
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("plan");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 5
title = "Operator decision"
description = "Surface deferred rows for operator decision."
test = "not testable - orchestration handoff"
estimated_tokens = 10
"#,
        )
        .expect("tasks");
        std::fs::write(
            round_dir.join("recovery.toml"),
            r#"status = "revise"
trigger = "human_blocked"
interactive = false
summary = "Recovery cannot autonomously close the deferred inventory rows."
feedback = ["Ask the operator whether to open a follow-up campaign or close the rows."]
changed_files = ["artifacts/spec.md", "artifacts/plan.md", "artifacts/tasks.toml"]
"#,
        )
        .expect("recovery");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(7);
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(7),
            status: PipelineItemStatus::Running,
            title: Some("Agent pivot recovery".to_string()),
            mode: None,
            trigger: Some("agent_pivot".to_string()),
            interactive: Some(false),
        });
        let run = RunRecord {
            id: 77,
            stage: "recovery".to_string(),
            task_id: None,
            round: 7,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Recovery]".to_string(),
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

        let reason = app
            .normalized_failure_reason(&run)
            .expect("normalized")
            .expect("revise status must fail recovery run");

        assert!(reason.starts_with("recovery_requested_revise:"), "{reason}");
        let recovery_item = app
            .state
            .builder
            .pipeline_items
            .iter()
            .find(|item| item.stage == "recovery")
            .expect("recovery pipeline item");
        assert_eq!(recovery_item.trigger.as_deref(), Some("human_blocked"));
        assert_eq!(recovery_item.interactive, Some(true));
    });
}

#[test]
fn recovery_reconcile_replaces_pending_and_sets_retry_reset_cutoff() {
    with_temp_root(|| {
        let session_id = "recovery-reconcile-success";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("002");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            artifacts.join("spec.md"),
            "Spec\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
        )
        .expect("spec");
        std::fs::write(
            artifacts.join("plan.md"),
            "Plan\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
        )
        .expect("plan");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 2
title = "Finish task 2"
description = "do it"
test = "cargo test"
estimated_tokens = 10

[[tasks]]
id = 5
title = "New follow-up"
description = "new work"
test = "cargo test"
estimated_tokens = 10
"#,
        )
        .expect("tasks");
        std::fs::write(
            round_dir.join("recovery.toml"),
            r#"status = "approved"
trigger = "agent_pivot"
interactive = false
summary = "recovered queue"
feedback = ["split task 2"]
changed_files = ["artifacts/spec.md", "artifacts/plan.md", "artifacts/tasks.toml"]
"#,
        )
        .expect("recovery");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        state.builder.done = vec![1, 4];
        state.builder.pending = vec![2, 3];
        state.builder.current_task = Some(2);
        state.builder.recovery_prev_max_task_id = Some(4);
        state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4];
        state.agent_runs.push(RunRecord {
            id: 7,
            stage: "coder".to_string(),
            task_id: Some(2),
            round: 2,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder]".to_string(),
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
            stage: "recovery".to_string(),
            task_id: None,
            round: 2,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Recovery]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let mut app = idle_app(state);
        let run = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 8)
            .cloned()
            .expect("recovery run");
        app.finalize_current_run(&run).expect("finalize recovery");

        // Recovery now routes through plan-review → sharding before implementation.
        assert_eq!(app.state.current_phase, Phase::BuilderRecoveryPlanReview(2));
        assert_eq!(app.state.builder.done, vec![1, 4]);
        assert_eq!(app.state.builder.pending, vec![2, 5]);
        assert_eq!(app.state.builder.current_task, None);
        assert_eq!(app.state.builder.retry_reset_run_id_cutoff, Some(8));
        assert_eq!(app.state.builder.recovery_trigger_task_id, None);
        assert_eq!(app.state.builder.recovery_prev_max_task_id, None);
        assert!(app.state.builder.recovery_prev_task_ids.is_empty());
        assert_eq!(app.state.builder.recovery_trigger_summary, None);
    });
}

#[test]
fn recovery_reconcile_requires_notes_for_superseded_started_tasks() {
    with_temp_root(|| {
        let session_id = "recovery-reconcile-notes";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(artifacts.join("spec.md"), "Spec without section").expect("spec");
        std::fs::write(artifacts.join("plan.md"), "Plan without section").expect("plan");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 6
title = "Replacement"
description = "replace task 2"
test = "cargo test"
estimated_tokens = 10
"#,
        )
        .expect("tasks");

        let mut state = SessionState::new(session_id.to_string());
        state.builder.done = vec![1];
        state.builder.recovery_prev_max_task_id = Some(5);
        state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4, 5];
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(2),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder]".to_string(),
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
        let err = app
            .reconcile_builder_recovery(99)
            .expect_err("expected supersession rejection");
        let text = format!("{err:#}");
        assert!(text.contains("Recovery Notes"));
    });
}

#[test]
fn recovery_auto_launch_is_idempotent_on_resume() {
    with_temp_root(|| {
        let session_id = "recovery-resume-autolaunch";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(artifacts.join("spec.md"), "spec").expect("spec");
        std::fs::write(artifacts.join("plan.md"), "plan").expect("plan");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 1
title = "Task"
description = "d"
test = "t"
estimated_tokens = 1
"#,
        )
        .expect("tasks");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(1);
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

        app.maybe_auto_launch();
        let first_run_count = app.state.agent_runs.len();
        assert_eq!(first_run_count, 1);
        assert_eq!(app.state.agent_runs[0].stage, "recovery");

        app.maybe_auto_launch();
        assert_eq!(app.state.agent_runs.len(), first_run_count);
    });
}

#[test]
fn circuit_breaker_escalates_to_human_blocked_after_3_cycles() {
    with_temp_root(|| {
        let mut state = SessionState::new("circuit-breaker-test".to_string());
        state.current_phase = Phase::ReviewRound(1);
        let session_dir = session_state::session_dir("circuit-breaker-test");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut app = idle_app(state);

        // First call: agent_pivot (cycle 1)
        app.enter_builder_recovery(1, None, None, "agent_pivot");
        {
            let recovery_items: Vec<_> = app
                .state
                .builder
                .pipeline_items
                .iter()
                .filter(|i| i.stage == "recovery")
                .collect();
            assert_eq!(recovery_items[0].trigger.as_deref(), Some("agent_pivot"));
            assert_eq!(app.state.builder.recovery_cycle_count, 1);
        }

        // Remove the recovery item and reset phase for second call
        app.state
            .builder
            .pipeline_items
            .retain(|i| i.stage != "recovery");
        app.state.current_phase = Phase::ReviewRound(1);
        // write tasks.toml again since recovery may clear state
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        // Second call: agent_pivot (cycle 2)
        app.enter_builder_recovery(1, None, None, "agent_pivot");
        assert_eq!(app.state.builder.recovery_cycle_count, 2);
        {
            let recovery_items: Vec<_> = app
                .state
                .builder
                .pipeline_items
                .iter()
                .filter(|i| i.stage == "recovery")
                .collect();
            assert_eq!(recovery_items[0].trigger.as_deref(), Some("agent_pivot"));
        }

        // Remove and reset for third call
        app.state
            .builder
            .pipeline_items
            .retain(|i| i.stage != "recovery");
        app.state.current_phase = Phase::ReviewRound(1);
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        // Third call: agent_pivot → should escalate to human_blocked
        app.enter_builder_recovery(1, None, None, "agent_pivot");
        assert_eq!(app.state.builder.recovery_cycle_count, 3);
        {
            let recovery_items: Vec<_> = app
                .state
                .builder
                .pipeline_items
                .iter()
                .filter(|i| i.stage == "recovery")
                .collect();
            // Must be escalated to human_blocked
            assert_eq!(
                recovery_items[0].trigger.as_deref(),
                Some("human_blocked"),
                "3rd cycle must escalate to human_blocked"
            );
            assert_eq!(recovery_items[0].interactive, Some(true));
        }
    });
}

#[test]
fn circuit_breaker_already_human_blocked_does_not_double_escalate() {
    with_temp_root(|| {
        let mut state = SessionState::new("circuit-breaker-hb".to_string());
        state.current_phase = Phase::ReviewRound(1);
        // Start with count=2 to be just below threshold
        state.builder.recovery_cycle_count = 2;
        let session_dir = session_state::session_dir("circuit-breaker-hb");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut app = idle_app(state);
        // Count becomes 3, trigger is already human_blocked — no double-escalation message
        app.enter_builder_recovery(1, None, None, "human_blocked");
        assert_eq!(app.state.builder.recovery_cycle_count, 3);
        let recovery_items: Vec<_> = app
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|i| i.stage == "recovery")
            .collect();
        // Stays human_blocked
        assert_eq!(recovery_items[0].trigger.as_deref(), Some("human_blocked"));
    });
}

#[test]
fn circuit_breaker_resets_after_approved_plan_review() {
    // Verify that recovery_cycle_count is reset to 0 when the recovery
    // plan review is approved (see handle_recovery_plan_review_completed).
    let mut builder = crate::state::BuilderState {
        recovery_cycle_count: 3,
        ..crate::state::BuilderState::default()
    };
    // Simulate the reset that happens in handle_recovery_plan_review_completed
    builder.recovery_cycle_count = 0;
    assert_eq!(builder.recovery_cycle_count, 0);
}

#[test]
fn recovery_queue_validation_rejects_completed_id_collision() {
    // reconcile_builder_recovery must reject recovered task ids that
    // collide with completed task ids.
    with_temp_root(|| {
        let session_id = "recovery-collision";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&round_dir).unwrap();

        // Write a recovery.toml
        std::fs::write(
            round_dir.join("recovery.toml"),
            "status = \"approved\"\nsummary = \"Fixed\"\nfeedback = []\n",
        )
        .unwrap();
        // Write spec.md and plan.md (no recovery notes needed since no superseded started ids)
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();

        // tasks.toml has task id 1 which is ALREADY done
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        state.builder.done = vec![1]; // task 1 is already done

        // Add a recovery pipeline item marked Running
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: None,
            mode: None,
            trigger: Some("agent_pivot".to_string()),
            interactive: Some(false),
        });
        let app = idle_app(state);
        // The reconcile should fail because task 1 is already completed
        // but the recovered tasks.toml also has task 1.
        let mut app = app;
        let result = app.reconcile_builder_recovery(0);
        assert!(result.is_err(), "collision with completed id must fail");
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("completed task id"),
            "error must mention collision: {msg}"
        );
    });
}

#[test]
fn recovery_queue_reconcile_preserves_completed_tasks() {
    with_temp_root(|| {
        let session_id = "recovery-preserve";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&round_dir).unwrap();

        std::fs::write(
            round_dir.join("recovery.toml"),
            "status = \"approved\"\nsummary = \"Fixed\"\nfeedback = []\n",
        )
        .unwrap();
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();

        // Recovered tasks.toml has ids 5 and 6 (new, above old max of 2)
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 5\ntitle = \"New A\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n\
                 [[tasks]]\nid = 6\ntitle = \"New B\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        // Tasks 1 and 2 are completed
        state.builder.done = vec![1, 2];
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Old Task 1".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(2),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Old Task 2".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: None,
            mode: None,
            trigger: Some("agent_pivot".to_string()),
            interactive: Some(false),
        });
        state.builder.recovery_prev_max_task_id = Some(2);
        state.builder.sync_legacy_queue_views();

        let mut app = idle_app(state);
        app.reconcile_builder_recovery(0)
            .expect("reconcile must succeed");

        // Completed tasks 1 and 2 must still be present
        let done = app.state.builder.done_task_ids();
        assert!(done.contains(&1));
        assert!(done.contains(&2));

        // New tasks 5 and 6 must be pending
        let pending = app.state.builder.pending_task_ids();
        assert!(pending.contains(&5));
        assert!(pending.contains(&6));
    });
}

#[test]
fn approved_review_with_feedback_emits_advisory_message() {
    with_temp_root(|| {
        let session_id = "approved-advisory";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&round_dir).unwrap();

        // Write tasks.toml with one task
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        // Write an approved review with non-empty feedback (advisory)
        std::fs::write(
                round_dir.join("review.toml"),
                "status = \"approved\"\nsummary = \"Implementation is correct\"\nfeedback = [\"Consider caching the result for performance\"]\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ReviewRound(1);
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: Some("Task 1".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });
        state.builder.sync_legacy_queue_views();
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "reviewer".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[Review r1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.current_run_id = Some(1);
        app.run_launched = true;

        write_finish_stamp(
            &session_dir,
            &App::run_key_for("reviewer", Some(1), 1, 1),
            "head789",
            "stable",
        );

        app.poll_agent_run();

        // The pipeline should still advance (not halted by advisory feedback)
        assert!(
            matches!(
                app.state.current_phase,
                Phase::ImplementationRound(_) | Phase::Done
            ),
            "Approved verdict must advance pipeline, got {:?}",
            app.state.current_phase
        );

        // An advisory message must have been emitted
        let advisory_msgs: Vec<_> = app
            .messages
            .iter()
            .filter(|m| m.kind == MessageKind::SummaryWarn)
            .filter(|m| m.text.contains("advisory"))
            .collect();
        assert!(
            !advisory_msgs.is_empty(),
            "advisory feedback must be surfaced as SummaryWarn message"
        );
    });
}

#[test]
fn recovery_prompt_interactive_requires_operator_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    let prompt = recovery_prompt(
        &tmp.path().join("spec.md"),
        &tmp.path().join("plan.md"),
        &tmp.path().join("tasks.toml"),
        Some(1),
        Some("needs human judgment"),
        &[],
        &[1],
        &tmp.path().join("live_summary.txt"),
        &tmp.path().join("recovery.toml"),
        true,
    );
    assert!(
        prompt.contains("INTERACTIVE"),
        "human_blocked prompt must be marked INTERACTIVE"
    );
    assert!(
        !prompt.contains("NON-INTERACTIVE"),
        "human_blocked prompt must not contain NON-INTERACTIVE"
    );
    assert!(
        prompt.contains("wait for explicit\n    confirmation"),
        "human_blocked prompt must require operator confirmation"
    );
    assert!(
        prompt.contains("`/exit`"),
        "interactive recovery prompt must ask the operator to enter /exit"
    );
}

#[test]
fn recovery_prompt_non_interactive_for_agent_pivot() {
    let tmp = tempfile::tempdir().unwrap();
    let prompt = recovery_prompt(
        &tmp.path().join("spec.md"),
        &tmp.path().join("plan.md"),
        &tmp.path().join("tasks.toml"),
        Some(2),
        Some("plan is wrong"),
        &[],
        &[2],
        &tmp.path().join("live_summary.txt"),
        &tmp.path().join("recovery.toml"),
        false,
    );
    assert!(
        prompt.contains("NON-INTERACTIVE"),
        "agent_pivot prompt must be NON-INTERACTIVE"
    );
    assert!(
        !prompt.contains("INTERACTIVE — the operator"),
        "agent_pivot prompt must not be marked INTERACTIVE"
    );
    assert!(
        !prompt.contains("`/exit`"),
        "non-interactive recovery prompt must not include /exit instruction"
    );
}

#[test]
fn normalize_failure_reason_pending_decision_parks_run() {
    with_temp_root(|| {
        let session_id = "pending-guard-park";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let run = make_brainstorm_run(42);
        state.agent_runs.push(run.clone());
        let mut app = mk_app(state);

        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
            .expect("write spec");

        write_finish_stamp_for_run(&app, &run, 0, "");
        write_ask_operator_snapshot(&session_dir);

        let result = app.normalized_failure_reason(&run).expect("call ok");
        assert!(
            result.is_none(),
            "PendingDecision must not become a hard failure reason, got: {result:?}"
        );
        let decision = app
            .state
            .pending_guard_decision
            .as_ref()
            .expect("pending_guard_decision must be Some after PendingDecision");
        assert_eq!(decision.run_id, run.id);
        assert_eq!(decision.stage, "brainstorm");
        assert_eq!(
            decision.captured_head,
            "0000000000000000000000000000000000000000"
        );
    });
}

#[test]
fn finalize_current_run_transitions_to_git_guard_pending() {
    with_temp_root(|| {
        let session_id = "pending-guard-finalize";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let run = make_brainstorm_run(1);
        state.agent_runs.push(run.clone());
        let mut app = mk_app(state);

        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
            .expect("write spec");
        write_finish_stamp_for_run(&app, &run, 0, "");
        write_ask_operator_snapshot(&session_dir);

        app.finalize_current_run(&run).expect("finalize ok");
        assert_eq!(
            app.state.current_phase,
            Phase::GitGuardPending,
            "phase must be GitGuardPending after parked run"
        );
        assert!(
            app.state.pending_guard_decision.is_some(),
            "pending_guard_decision must be set"
        );
    });
}

#[test]
fn orphan_live_summary_files_removed_at_session_start() {
    with_temp_root(|| {
        let session_id = "orphan-live-summary-sweep";
        let session_dir = session_state::session_dir(session_id);
        let artifacts_dir = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(RunRecord {
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
        });

        let live_txt = artifacts_dir.join("live_summary.txt");
        std::fs::write(&live_txt, "stale").expect("write live_summary.txt");
        let running_key = App::run_key_for("brainstorm", None, 1, 1);
        let running_path = artifacts_dir.join(format!("live_summary.{running_key}.txt"));
        std::fs::write(&running_path, "running").expect("write running live_summary");
        let orphan_path = artifacts_dir.join("live_summary.orphan.txt");
        std::fs::write(&orphan_path, "orphan").expect("write orphan live_summary");

        assert!(live_txt.exists());
        assert!(running_path.exists());
        assert!(orphan_path.exists());

        let _app = App::new(state);

        assert!(
            !live_txt.exists(),
            "pointer live_summary.txt must be removed at startup"
        );
        assert!(
            running_path.exists(),
            "live_summary.<run_key>.txt for Running record must be retained"
        );
        assert!(
            !orphan_path.exists(),
            "orphan live_summary.<run_key>.txt must be removed at startup"
        );
    });
}

#[test]
fn resume_missing_window_honors_present_finish_stamp_for_coder() {
    with_temp_root(|| {
        let session_id = "resume-coder-stamp-present";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Builder r1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "after",
            "stable",
        );

        let resumed = state
            .resume_running_runs()
            .expect("resume")
            .expect("run id");

        let mut app = idle_app(state);
        app.current_run_id = Some(resumed);
        app.run_launched = true;
        app.poll_agent_run();

        let run = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run");
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(app.state.current_phase, Phase::ReviewRound(1));
    });
}

#[test]
fn resume_missing_window_missing_stamp_fails_unverified_for_coder() {
    with_temp_root(|| {
        let session_id = "resume-coder-stamp-missing";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Builder r1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let resumed = state
            .resume_running_runs()
            .expect("resume")
            .expect("run id");

        let mut app = idle_app(state);
        app.current_run_id = Some(resumed);
        app.run_launched = true;
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
        app.poll_agent_run();

        let run = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run");
        assert_eq!(run.status, RunStatus::FailedUnverified);
        assert!(
            run.error
                .as_deref()
                .unwrap_or_default()
                .contains("missing finish stamp"),
            "must fail closed on missing stamp"
        );
        assert_eq!(app.state.current_phase, Phase::ImplementationRound(1));
    });
}

#[test]
fn resume_missing_window_missing_stamp_warns_and_finalizes_for_non_coder() {
    with_temp_root(|| {
        let session_id = "resume-planning-stamp-missing";
        let session_dir = session_state::session_dir(session_id);
        let artifacts_dir = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        std::fs::write(artifacts_dir.join("plan.md"), "# Plan\n").expect("write plan");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let resumed = state
            .resume_running_runs()
            .expect("resume")
            .expect("run id");

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(resumed);
        app.run_launched = true;
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
        app.poll_agent_run();

        let run = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run");
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(app.state.current_phase, Phase::PlanReviewRunning);

        let warned = app.messages.iter().any(|m| {
            m.kind == MessageKind::SummaryWarn && m.text.contains("finish_stamp_missing:")
        });
        assert!(
            warned,
            "non-coder missing stamp must warn on barrier release"
        );
    });
}

#[test]
fn stamp_archival_moves_old_stamps_at_session_start() {
    use crate::runner::{FinishStamp, write_finish_stamp};

    with_temp_root(|| {
        let session_id = "stamp-archival-test";
        let mut state = SessionState::new(session_id.to_string());

        let old_time = chrono::Utc::now() - chrono::Duration::hours(2);
        let recent_time = chrono::Utc::now() - chrono::Duration::minutes(5);

        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Builder 1]".to_string(),
            started_at: recent_time,
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.save().unwrap();

        let finish_dir = session_state::session_dir(session_id)
            .join("artifacts")
            .join("run-finish");
        std::fs::create_dir_all(&finish_dir).unwrap();

        let old_stamp = FinishStamp {
            finished_at: old_time.to_rfc3339(),
            exit_code: 0,
            head_before: "aaa".to_string(),
            head_after: "bbb".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
            working_tree_clean: true,
        };
        let old_stamp_path = finish_dir.join("old-stamp.toml");
        write_finish_stamp(&old_stamp_path, &old_stamp).unwrap();

        let recent_stamp = FinishStamp {
            finished_at: recent_time.to_rfc3339(),
            exit_code: 0,
            head_before: "ccc".to_string(),
            head_after: "ddd".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
            working_tree_clean: true,
        };
        let recent_stamp_path = finish_dir.join("recent-stamp.toml");
        write_finish_stamp(&recent_stamp_path, &recent_stamp).unwrap();

        assert!(
            old_stamp_path.exists(),
            "old stamp should exist before App creation"
        );
        assert!(
            recent_stamp_path.exists(),
            "recent stamp should exist before App creation"
        );

        // Create App which triggers archival
        let _app = App::new(state);

        let archive_dir = finish_dir.join("archive");
        if !old_stamp_path.exists() {
            // Stamp was archived
            assert!(
                archive_dir.exists(),
                "archive directory should be created when stamps are archived"
            );
            assert!(
                archive_dir.join("old-stamp.toml").exists(),
                "old stamp should be moved to archive"
            );
        }
        assert!(
            recent_stamp_path.exists(),
            "recent stamp should remain in main directory"
        );
    });
}

#[test]
fn archived_stamps_not_consulted_by_coder_gate() {
    use crate::runner::{FinishStamp, write_finish_stamp};

    with_temp_root(|| {
        let session_id = "archived-stamp-ignore";
        let mut state = SessionState::new(session_id.to_string());

        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Builder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.save().unwrap();

        let finish_dir = session_state::session_dir(session_id)
            .join("artifacts")
            .join("run-finish");
        let archive_dir = finish_dir.join("archive");
        std::fs::create_dir_all(&archive_dir).unwrap();

        let run_key = App::run_key_for("coder", Some(1), 1, 1);
        let archived_stamp_path = archive_dir.join(format!("{run_key}.toml"));
        let archived_stamp = FinishStamp {
            finished_at: chrono::Utc::now().to_rfc3339(),
            exit_code: 0,
            head_before: "base".to_string(),
            head_after: "advanced".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
            working_tree_clean: true,
        };
        write_finish_stamp(&archived_stamp_path, &archived_stamp).unwrap();

        let round_dir = session_state::session_dir(session_id)
            .join("rounds")
            .join("001");
        std::fs::create_dir_all(&round_dir).unwrap();
        std::fs::write(round_dir.join("review_scope.toml"), "base_sha = \"base\"\n").unwrap();

        let app = App::new(SessionState::load(session_id).unwrap());
        let run = &app.state.agent_runs[0];
        let reason = app.coder_gate_reason(run, &round_dir);

        assert!(
            reason.is_some(),
            "archived stamp must not be consulted; should return failure reason"
        );
        assert!(
            reason.unwrap().contains("missing finish stamp"),
            "should report missing stamp, not use archived one"
        );
    });
}
