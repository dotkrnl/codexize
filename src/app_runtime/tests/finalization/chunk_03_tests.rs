use super::*;

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
            section_path: None,
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

#[test]
fn queue_empty_approved_review_enters_simplification_when_not_yolo() {
    with_temp_root(|| {
        let session_id = "review-to-simplification";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"Task 1\"\ndescription = \"d\"\ntest = \"cargo test\"\nestimated_tokens = 100\n",
        )
        .expect("tasks");
        std::fs::write(
            round_dir.join("review.toml"),
            "status = \"approved\"\nsummary = \"done\"\n",
        )
        .expect("review");

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
            "head123",
            "stable",
        );

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::Simplification(1));
        // Simplification gates entry into FinalValidation; the validation
        // counter must not advance until the simplifier completes.
        assert_eq!(app.state.validation_attempts, 0);
        assert_eq!(
            app.state.simplification_attempts.get(&1).copied(),
            Some(1),
            "simplification attempt counter must advance on entry"
        );
        assert_eq!(app.state.builder.done_task_ids(), vec![1]);
        assert!(!app.state.builder.has_unfinished_tasks());
    });
}

#[test]
fn queue_empty_approved_review_bypasses_simplification_in_yolo() {
    with_temp_root(|| {
        let session_id = "review-to-done-yolo";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("review.toml"),
            "status = \"approved\"\nsummary = \"done\"\n",
        )
        .expect("review");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ReviewRound(1);
        state.builder.current_task = Some(1);
        let mut run = RunRecord {
            id: 11,
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
        };
        run.modes.yolo = true;
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::Done);
        assert_eq!(app.state.validation_attempts, 0);
    });
}

#[test]
fn skip_to_impl_coder_completion_enters_simplification_when_not_yolo() {
    with_temp_root(|| {
        let session_id = "skip-to-impl-simplification";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"Task 1\"\ndescription = \"d\"\ntest = \"cargo test\"\nestimated_tokens = 100\n",
        )
        .expect("tasks");
        write_review_scope(&round_dir, "base123");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.skip_to_impl_rationale = Some("small change".to_string());
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
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(1);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        // Skip-to-impl shares the simplification gate with the converging
        // loop path: round-1 implementation completes go through
        // `Simplification(1)` before any final validation.
        assert_eq!(app.state.current_phase, Phase::Simplification(1));
        assert_eq!(app.state.validation_attempts, 0);
        assert_eq!(app.state.simplification_attempts.get(&1).copied(), Some(1));
    });
}

#[test]
fn queue_empty_review_blocks_when_simplification_cap_is_already_exhausted() {
    with_temp_root(|| {
        let session_id = "review-simplification-cap";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("review.toml"),
            "status = \"approved\"\nsummary = \"done\"\n",
        )
        .expect("review");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ReviewRound(1);
        // Pre-exhaust the per-round simplifier cap so the entry guard routes
        // straight to BlockedNeedsUser instead of launching the simplifier.
        // Validation attempts must not advance from this branch — that cap
        // is only consumed once the simplifier hands control to
        // FinalValidation, which is gated separately.
        state
            .simplification_attempts
            .insert(1, session_state::transitions::SIMPLIFICATION_ATTEMPT_CAP);
        state.builder.current_task = Some(1);
        state.agent_runs.push(RunRecord {
            id: 12,
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
        app.current_run_id = Some(12);
        app.run_launched = true;
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("reviewer", Some(1), 1, 1),
            "head123",
            "stable",
        );

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        // Simplification blocks must populate `BlockOrigin::Simplification`
        // so the force-ship runtime guard (FinalValidation-only) stays shut.
        assert_eq!(
            app.state.block_origin,
            Some(crate::state::BlockOrigin::Simplification)
        );
        assert_eq!(app.state.validation_attempts, 0);
        assert_eq!(
            app.state.simplification_attempts.get(&1).copied(),
            Some(session_state::transitions::SIMPLIFICATION_ATTEMPT_CAP)
        );
    });
}

#[test]
fn final_validation_goal_met_transitions_to_done() {
    with_temp_root(|| {
        let session_id = "final-validation-goal-met";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(
            artifacts.join("final_validation_2.toml"),
            "status = \"goal_met\"\nsummary = \"all set\"\nfindings = [\"workspace clean\"]\n",
        )
        .expect("verdict");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(2);
        state.validation_attempts = 2;
        let run = RunRecord {
            id: 5,
            stage: "final-validation".to_string(),
            task_id: None,
            round: 2,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[FinalValidation]".to_string(),
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
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::Done);
        assert_eq!(app.state.validation_attempts, 2);
    });
}

#[test]
fn final_validation_goal_gap_appends_tasks_and_restarts_builder_loop() {
    with_temp_root(|| {
        let session_id = "final-validation-goal-gap";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"Initial task\"\ndescription = \"d\"\ntest = \"cargo test\"\nestimated_tokens = 100\n",
        )
        .expect("tasks");
        std::fs::write(
            artifacts.join("final_validation_1.toml"),
            r#"status = "goal_gap"
summary = "missing a follow-up"
findings = ["checked src/lib.rs"]

[[gaps]]
description = "missing follow-up"
checked = ["src/lib.rs"]

[[new_tasks]]
title = "Add follow-up"
description = "Implement the missing work"
test = "cargo test gap_follow_up"
estimated_tokens = 250
"#,
        )
        .expect("verdict");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(1);
        state.validation_attempts = 1;
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Initial task".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        state.builder.sync_legacy_queue_views();
        let run = RunRecord {
            id: 8,
            stage: "final-validation".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[FinalValidation]".to_string(),
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
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::ImplementationRound(2));
        assert_eq!(app.state.builder.pending_task_ids(), vec![2]);
        assert_eq!(app.state.builder.done_task_ids(), vec![1]);
        let parsed = tasks::validate(&artifacts.join("tasks.toml")).expect("tasks valid");
        assert_eq!(parsed.tasks.len(), 2);
        assert_eq!(parsed.tasks[1].id, 2);
        assert_eq!(parsed.tasks[1].title, "Add follow-up");
    });
}

#[test]
fn goal_gap_follow_up_review_reenters_simplification_then_final_validation() {
    with_temp_root(|| {
        let session_id = "final-validation-second-pass";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round2_dir = session_dir.join("rounds").join("002");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round2_dir).expect("round2 dir");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 1
title = "Initial task"
description = "d"
test = "cargo test"
estimated_tokens = 100

[[tasks]]
id = 2
title = "Follow-up"
description = "more work"
test = "cargo test follow_up"
estimated_tokens = 200
"#,
        )
        .expect("tasks");
        std::fs::write(
            round2_dir.join("review.toml"),
            "status = \"approved\"\nsummary = \"follow-up done\"\n",
        )
        .expect("review");
        std::fs::write(
            artifacts.join("final_validation_2.toml"),
            "status = \"goal_met\"\nsummary = \"fully complete\"\nfindings = [\"workspace clean\"]\n",
        )
        .expect("verdict");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ReviewRound(2);
        state.validation_attempts = 1;
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Initial task".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(2),
            round: Some(2),
            status: PipelineItemStatus::Running,
            title: Some("Follow-up".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        state.builder.sync_legacy_queue_views();
        let review_run = RunRecord {
            id: 13,
            stage: "reviewer".to_string(),
            task_id: Some(2),
            round: 2,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[Review r2]".to_string(),
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
        state.agent_runs.push(review_run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(review_run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &review_run, 0, "");

        app.poll_agent_run();
        // After the goal-gap rerun, the converged ReviewRound(2) routes
        // through Simplification(2) before reaching FinalValidation(2).
        assert_eq!(app.state.current_phase, Phase::Simplification(2));
        assert_eq!(app.state.validation_attempts, 1);
        assert_eq!(app.state.simplification_attempts.get(&2).copied(), Some(1));

        // Simulate the simplifier handing control back to FinalValidation;
        // launch/finalization for the simplifier itself lands in a
        // follow-up task, but the post-simplifier transition is fixed by
        // the state graph.
        let outcome = session_state::transitions::enter_final_validation(&mut app.state, 2)
            .expect("post-simplifier final validation entry");
        assert!(matches!(
            outcome,
            session_state::transitions::FinalValidationEntry::Entered { attempt: 2 }
        ));
        assert_eq!(app.state.current_phase, Phase::FinalValidation(2));
        assert_eq!(app.state.validation_attempts, 2);

        let validation_run = RunRecord {
            id: 14,
            stage: "final-validation".to_string(),
            task_id: None,
            round: 2,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[FinalValidation]".to_string(),
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
        app.state.agent_runs.push(validation_run.clone());
        app.current_run_id = Some(validation_run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &validation_run, 0, "");

        app.poll_agent_run();
        assert_eq!(app.state.current_phase, Phase::Done);
    });
}

#[test]
fn final_validation_needs_human_blocks_with_final_validation_origin() {
    with_temp_root(|| {
        let session_id = "final-validation-needs-human";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(
            artifacts.join("final_validation_3.toml"),
            r#"status = "needs_human"
summary = "ambiguous requirement"
findings = ["checked spec and workspace"]

[[gaps]]
description = "operator input required"
checked = ["artifacts/spec.md"]
"#,
        )
        .expect("verdict");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(3);
        state.validation_attempts = 3;
        let run = RunRecord {
            id: 9,
            stage: "final-validation".to_string(),
            task_id: None,
            round: 3,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[FinalValidation]".to_string(),
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
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(
            app.state.block_origin,
            Some(crate::state::BlockOrigin::FinalValidation)
        );
    });
}
