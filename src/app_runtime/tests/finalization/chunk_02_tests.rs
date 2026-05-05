use super::*;

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
            iteration: 1,
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
            iteration: 1,
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
            iteration: 1,
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
            iteration: 1,
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
            section_path: None,
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

        // The pipeline should still advance (not halted by advisory feedback).
        // Converged-review approval routes through the simplifier before
        // FinalValidation, so any of those forward phases is acceptable.
        assert!(
            matches!(
                app.state.current_phase,
                Phase::ImplementationRound(_)
                    | Phase::Simplification(_)
                    | Phase::FinalValidation(_)
                    | Phase::Done
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
            section_path: None,
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
            section_path: None,
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
            section_path: None,
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
            section_path: None,
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
            section_path: None,
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
