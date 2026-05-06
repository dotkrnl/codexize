use super::*;

#[test]
fn enter_builder_recovery_from_block_bumps_iteration() {
    with_temp_root(|| {
        let session_id = "enter-recovery-from-block-iter";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"Task 1\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 10\n",
        )
        .expect("tasks");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(crate::state::BlockOrigin::FinalValidation);
        // One coder pipeline item at iteration 1.
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Task 1".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });

        let mut app = idle_app(state);
        app.enter_builder_recovery_from_block();

        assert!(
            matches!(app.state.current_phase, Phase::BuilderRecovery(_)),
            "must transition into BuilderRecovery; phase is {:?}",
            app.state.current_phase
        );

        // The override must have been set to max_iteration + 1 = 2.
        // It is NOT consumed during enter_builder_recovery; it is consumed
        // later by recovery_outer_iteration() during reconcile.
        assert_eq!(
            app.state.builder.next_iteration_for_recovery,
            Some(2),
            "override must be set to 2 (max pipeline iteration 1 + 1) for later reconcile consumption"
        );
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
            section_path: None,
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
            section_path: None,
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
            iteration: 1,
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
            section_path: None,
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
            section_path: None,
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
            section_path: None,
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
            section_path: None,
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
fn recovery_outer_iteration_consumes_one_shot_override() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-iter-override".to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        // Add a coder pipeline item at iteration 2 so fallback would return 2.
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Task 1".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 2,
        });
        state.builder.next_iteration_for_recovery = Some(5);
        let mut app = idle_app(state);
        let iter = app.recovery_outer_iteration();
        assert_eq!(iter, 5, "must read the override");
        assert_eq!(
            app.state.builder.next_iteration_for_recovery, None,
            "override is consumed once"
        );
    });
}

#[test]
fn recovery_outer_iteration_falls_back_when_no_override() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-iter-fallback".to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        state.builder.recovery_trigger_task_id = Some(1);
        // Add a coder pipeline item with task_id=1 at iteration 2.
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("Task 1".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 2,
        });
        // next_iteration_for_recovery is None (default) — fallback to trigger task
        let mut app = idle_app(state);
        let iter = app.recovery_outer_iteration();
        assert_eq!(iter, 2, "trigger task's iteration when no override");
    });
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
            iteration: 1,
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
