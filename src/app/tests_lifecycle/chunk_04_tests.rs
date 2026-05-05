use super::*;

#[test]
fn cancel_modal_command_clears_quit_confirmation_run_active() {
    with_temp_root(|| {
        let mut state = SessionState::new("quit-modal-cancel-cmd".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(11));
        let mut app = mk_app(state);
        app.current_run_id = Some(11);
        app.pending_quit_confirmation_run_id = Some(11);

        let should_quit = app.handle_app_command(crate::app_runtime::AppCommand::CancelModal);

        assert!(!should_quit);
        assert!(
            app.pending_quit_confirmation_run_id.is_none(),
            "CancelModal must clear the pending quit confirmation"
        );
        assert_eq!(app.active_modal(), None);
        assert!(app.has_running_agent());
    });
}

#[test]
fn pending_guard_resume_fail_closed_when_decision_missing() {
    with_temp_root(|| {
        let session_id = "pending-guard-resume-fail";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        state.builder.recovery_trigger_task_id = Some(2);
        state.builder.recovery_prev_max_task_id = Some(4);
        state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4];
        state.builder.recovery_trigger_summary = Some("stale guard context".to_string());
        state.save().expect("save");

        let app = App::new(SessionState::load(session_id).expect("load session"));
        assert_eq!(
            app.state.current_phase,
            Phase::BlockedNeedsUser,
            "must fail closed to BlockedNeedsUser"
        );
        assert!(
            app.state.agent_error.is_some(),
            "agent_error must be set on fail-closed"
        );
        assert_eq!(app.state.builder.recovery_trigger_task_id, None);
        assert_eq!(app.state.builder.recovery_prev_max_task_id, None);
        assert!(app.state.builder.recovery_prev_task_ids.is_empty());
        assert_eq!(app.state.builder.recovery_trigger_summary, None);
    });
}

#[test]
fn pending_guard_resume_restores_modal_when_decision_present() {
    with_temp_root(|| {
        let session_id = "pending-guard-resume-ok";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 99,
            captured_head: "abc".to_string(),
            current_head: "def".to_string(),
            warnings: vec![],
        });
        state.save().expect("save");

        let app = App::new(SessionState::load(session_id).expect("load session"));
        assert_eq!(app.state.current_phase, Phase::GitGuardPending);
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn pending_guard_stale_decision_cleared_on_resume() {
    with_temp_root(|| {
        let session_id = "pending-guard-stale";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 77,
            captured_head: "aaa".to_string(),
            current_head: "bbb".to_string(),
            warnings: vec![],
        });
        state.save().expect("save");

        let app = App::new(SessionState::load(session_id).expect("load session"));
        assert!(
            app.state.pending_guard_decision.is_none(),
            "stale pending_guard_decision must be cleared on resume"
        );
        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
    });
}

#[test]
fn non_yolo_prompts_keep_interactive_operator_cues() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("stage-completion-prompts");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let recovery_path = artifacts.join("recovery.toml");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        let brainstorm = brainstorm_prompt(
            "add a feature",
            &spec_path.display().to_string(),
            &summary_path.display().to_string(),
            &live_summary.display().to_string(),
            false,
        );
        assert!(!brainstorm.contains("You have the operator's full trust."));
        assert!(brainstorm.contains("Operator IS available for design questions"));
        assert!(
            brainstorm
                .contains("Stage completion — ONLY once all pending design questions are resolved")
        );
        assert!(brainstorm.contains(
            "While you are\nstill waiting for the operator's input, never include this cue."
        ));
        assert!(!brainstorm.contains("End your final message"));

        let planning = planning_prompt(&spec_path, &[], &plan_path, &live_summary, false);
        assert!(!planning.contains("You have the operator's full trust."));
        assert!(planning.contains("Escalation rules — ask the operator when:"));
        assert!(
            planning.contains("The feedback affects end-user-facing design (UI/UX, CLI behavior,")
        );
        assert!(planning.contains("The feedback is an internal design decision"));
        assert!(planning.contains("Cosmetic / trivial (typos, naming nits, formatting,"));
        assert!(
            !planning.contains("If a real trade-off exceeds your\nconfidence, ASK the operator")
        );
        assert!(
            planning.contains(
                "Stage completion — ONLY once all pending trade-off decisions are resolved"
            )
        );
        assert!(planning.contains(
            "While you are still waiting\nfor the operator's input, never include this cue."
        ));
        assert!(!planning.contains("End your final message"));

        let recovery = recovery_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            Some(1),
            Some("needs confirmation"),
            &[],
            &[1],
            &live_summary,
            &recovery_path,
            true,
        );
        assert!(recovery.contains(
            "Stage completion — ONLY once all pending confirmation decisions are resolved"
        ));
        assert!(recovery.contains(
            "While you are\nstill waiting for the operator's confirmation, never include this cue."
        ));
        assert!(!recovery.contains("End your final message"));
    });
}

#[test]
fn spec_review_paused_enter_advances_regardless_of_selection() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::SpecReviewPaused;
        let mut app = idle_app(state);
        app.selected = 999;
        app.handle_key(key(crossterm::event::KeyCode::Enter));
        assert_eq!(app.state.current_phase, Phase::PlanningRunning);
    });
}

#[test]
fn plan_review_paused_n_reruns_plan_review() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::PlanReviewPaused;
        let mut app = idle_app(state);
        app.selected = 999;
        app.handle_key(key(crossterm::event::KeyCode::Char('n')));
        assert_eq!(app.state.current_phase, Phase::PlanReviewRunning);
    });
}

#[test]
fn modal_up_down_space_no_state_mutation() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::SpecReviewPaused;
        let mut app = idle_app(state);
        app.selected = 0;

        for k in [
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyCode::Char('b'),
            crossterm::event::KeyCode::Char('e'),
        ] {
            app.handle_key(key(k));
            assert_eq!(app.state.current_phase, Phase::SpecReviewPaused);
            assert_eq!(app.selected, 0); // No scroll occurred
        }
    });
}

#[test]
fn stage_error_enter_relaunches_from_non_current_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_error = Some("something went wrong".to_string());
        let mut app = idle_app(state);
        app.selected = 999;
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

        app.handle_key(key(crossterm::event::KeyCode::Enter));
        assert!(app.state.agent_error.is_none());
        assert!(app.current_run_id.is_some());
        assert_eq!(app.state.current_phase, Phase::SpecReviewRunning);
    });
}

#[test]
fn resolve_split_target_run_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-run".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;

        let target = app.resolve_split_target_for_selected_row();
        assert_eq!(target, Some(super::split::SplitTarget::Run(7)));
    });
}

#[test]
fn resolve_split_target_idea_row() {
    with_temp_root(|| {
        let state = SessionState::new("split-idea".to_string());
        let mut app = idle_app(state);
        let idea_idx = row_index(&app, "Idea");
        app.selected = idea_idx;

        let target = app.resolve_split_target_for_selected_row();
        assert_eq!(target, Some(super::split::SplitTarget::Idea));
    });
}

#[test]
fn resolve_split_target_other_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-none".to_string());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        // Select "Spec Review" stage row (no run_id directly on it in this setup)
        let sr_idx = row_index(&app, "Spec Review");
        app.selected = sr_idx;

        let target = app.resolve_split_target_for_selected_row();
        assert_eq!(target, None);
    });
}

#[test]
fn enter_opens_run_split_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-enter-run".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Run(7)));
    });
}

#[test]
fn enter_opens_idea_split_target() {
    with_temp_root(|| {
        let state = SessionState::new("split-enter-idea".to_string());
        let mut app = idle_app(state);
        let idea_idx = row_index(&app, "Idea");
        app.selected = idea_idx;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
    });
}

#[test]
fn enter_does_not_toggle_close_same_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-no-toggle".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;
        app.split_target = Some(super::split::SplitTarget::Run(7));
        app.split_scroll_offset = 42;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Run(7)));
        assert_eq!(
            app.split_scroll_offset, 42,
            "scroll must be preserved on same-target Enter"
        );
    });
}

#[test]
fn enter_does_not_switch_target_when_split_is_already_open() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-switch".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.split_scroll_offset = 42;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
        assert_eq!(
            app.split_scroll_offset, 42,
            "split-open Enter should be consumed before tree target resolution"
        );
    });
}

#[test]
fn split_new_target_clamps_to_tail_position() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-default".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }

        app.open_split_target(super::split::SplitTarget::Run(7));
        let content_height = app.current_split_content_height();
        let expected_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(content_height);

        assert_eq!(
            app.split_scroll_offset, expected_tail,
            "new run targets should open at the tail view, not the transcript top"
        );
    });
}

#[test]
fn split_scroll_detach_preserves_offset_across_new_content() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-detach".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }
        app.open_split_target(super::split::SplitTarget::Run(7));
        let content_height = app.current_split_content_height();
        let expected_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(content_height);

        app.handle_key(key(crossterm::event::KeyCode::Up));
        assert_eq!(
            app.split_scroll_offset,
            expected_tail.saturating_sub(1),
            "Up should detach from the tail"
        );
        let detached_offset = app.split_scroll_offset;

        app.messages.push(Message {
            ts: chrono::Utc::now(),
            run_id: 7,
            kind: MessageKind::UserInput,
            sender: MessageSender::System,
            text: "line 10".to_string(),
        });
        app.clamp_split_scroll(app.current_split_content_height());

        assert_eq!(
            app.split_scroll_offset, detached_offset,
            "new transcript content must not yank a detached split viewport back toward the tail"
        );
    });
}

#[test]
fn split_scroll_clamps_after_viewport_growth() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-clamp-grow".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        for idx in 0..15 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }
        app.open_split_target(super::split::SplitTarget::Run(7));
        let content_height = app.current_split_content_height();
        let expected_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(content_height);
        app.handle_key(key(crossterm::event::KeyCode::Up));
        app.handle_key(key(crossterm::event::KeyCode::Up));
        assert_eq!(app.split_scroll_offset, expected_tail.saturating_sub(2));

        app.body_inner_height = 18;
        let clamped_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            app.current_split_content_height(),
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(app.current_split_content_height());

        assert_eq!(
            app.split_scroll_offset, clamped_tail,
            "viewport changes should clamp detached offsets into the new valid range"
        );
    });
}

#[test]
fn split_open_space_does_not_toggle_tree_expansion() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-space-consumed".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;
        let expanded_before = app.is_expanded(bs_idx);
        app.open_split_target(super::split::SplitTarget::Run(7));

        app.handle_key(key(crossterm::event::KeyCode::Char(' ')));

        assert_eq!(
            app.is_expanded(bs_idx),
            expanded_before,
            "split-open transcript mode should consume Space before tree expansion logic"
        );
    });
}

#[test]
fn esc_closes_split_when_open() {
    with_temp_root(|| {
        let mut app = idle_app(SessionState::new("split-esc".to_string()));
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.split_scroll_offset = 5;

        let quit = app.handle_key(key(crossterm::event::KeyCode::Esc));

        assert!(!quit, "Esc must not quit while split is open");
        assert_eq!(app.split_target, None);
        assert_eq!(app.split_scroll_offset, 0);
    });
}

#[test]
fn poll_agent_run_closes_matching_interactive_run_split_on_exit() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-close-matching-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(RunRecord {
            id: 42,
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
            modes: crate::state::LaunchModes {
                interactive: true,
                ..Default::default()
            },
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(42);
        app.run_launched = true;
        app.split_target = Some(super::split::SplitTarget::Run(42));
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        assert_eq!(app.split_target, None);
    });
}

#[test]
fn poll_agent_run_preserves_switched_split_target_on_interactive_exit() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-preserve-switched-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(RunRecord {
            id: 42,
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
            modes: crate::state::LaunchModes {
                interactive: true,
                ..Default::default()
            },
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(42);
        app.run_launched = true;
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
    });
}

#[test]
fn esc_quits_when_split_closed_and_no_agent_running() {
    with_temp_root(|| {
        let mut app = idle_app(SessionState::new("split-esc-quit".to_string()));
        app.split_target = None;

        let quit = app.handle_key(key(crossterm::event::KeyCode::Esc));

        assert!(
            quit,
            "Esc should quit when split is closed and no agent running"
        );
    });
}

#[test]
fn rebuild_closes_invalid_run_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-rebuild".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.split_target = Some(super::split::SplitTarget::Run(7));
        app.split_scroll_offset = 3;

        // Remove the run without explicitly closing the split.
        app.state.agent_runs.retain(|run| run.id != 7);
        app.rebuild_tree_view(None);

        assert_eq!(
            app.split_target, None,
            "split must close when run disappears"
        );
        assert_eq!(app.split_scroll_offset, 0);
    });
}

#[test]
fn rebuild_preserves_idea_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-idea-preserved".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.split_scroll_offset = 3;

        app.rebuild_tree_view(None);

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
        assert_eq!(
            app.split_scroll_offset, 0,
            "Idea split scroll clamps because Idea content is currently non-scrollable"
        );
    });
}
