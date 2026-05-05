use super::*;

#[test]
fn split_follow_tail_reaches_latest_message_lines() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-visible-latest".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        if let Some(run) = state.agent_runs.iter_mut().find(|run| run.id == 7) {
            run.status = RunStatus::Done;
            run.ended_at = Some(chrono::Utc::now());
        }
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        app.body_inner_width = 80;
        app.open_split_target(super::split::SplitTarget::Run(7));

        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }

        app.clamp_split_scroll(app.current_split_content_height());
        let content_height = app.current_split_content_height();
        let window = crate::app::chat_widget_view_model::chat_scroll_window(
            content_height,
            app.split_viewport_height(),
            app.split_scroll_offset,
        )
        .expect("scroll window");

        assert_eq!(
            window.visible_end, content_height,
            "tail-follow should keep the newest transcript line in view"
        );
        assert!(
            window.offset > 0,
            "tail-follow should not reset new targets to the transcript top when content overflows"
        );
    });
}

#[test]
fn split_viewport_height_accounts_for_separator_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-viewport-separator".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(RunRecord {
            id: 7,
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
        let mut app = idle_app(state);
        app.split_target = Some(super::split::SplitTarget::Run(7));
        app.body_inner_height = 10;
        app.split_fullscreen = false;

        assert_eq!(
            app.split_viewport_height(),
            6,
            "non-fullscreen split viewport should match render allocation after the separator row"
        );
    });
}

#[test]
fn split_follow_tail_keeps_live_running_tail_visible() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-visible-running".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        app.body_inner_width = 80;
        app.selected = row_index(&app, "Brainstorm");
        app.open_split_target(super::split::SplitTarget::Run(7));

        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }

        let run = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == 7)
            .expect("run");
        let local_offset = chrono::Local::now().fixed_offset().offset().to_owned();
        let rendered_total = crate::app::chat_widget::message_lines(
            &app.messages,
            run,
            &local_offset,
            Some(ratatui::text::Line::from("LIVE-TAIL")),
            app.body_inner_width.max(1),
            0,
            true,
        )
        .len();

        app.clamp_split_scroll(app.current_split_content_height());
        let window = crate::app::chat_widget_view_model::chat_scroll_window(
            rendered_total,
            app.split_viewport_height(),
            app.split_scroll_offset,
        )
        .expect("scroll window");

        assert_eq!(
            window.visible_end, rendered_total,
            "tail-follow should keep the rendered live tail visible for running transcripts"
        );
        assert!(
            !window.show_below_indicator,
            "follow-tail should not leave newer rendered transcript lines below the split viewport"
        );
    });
}

#[test]
fn synchronize_split_target_does_not_auto_open_for_non_interactive_run() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-interactive-no-auto-open".to_string());
        state.current_phase = Phase::PlanningRunning;
        state
            .agent_runs
            .push(make_non_interactive_run(42, "non-int-1"));

        // Even with the runner label flagged as waiting for input, a
        // non-interactive run must not trigger auto-open, auto-switch, or
        // forced input focus.
        crate::runner::request_run_label_interactive_input_for_test("non-int-1");

        let mut app = idle_app(state);
        app.current_run_id = Some(42);

        assert!(app.split_target.is_none());
        assert!(!app.input_mode);

        app.synchronize_split_target();

        assert!(
            app.split_target.is_none(),
            "non-interactive run must not auto-open the split"
        );
        assert!(
            !app.input_mode,
            "non-interactive run must not force input focus"
        );
    });
}

#[test]
fn synchronize_split_target_does_not_force_focus_for_non_interactive_open_split() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-interactive-manual-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state
            .agent_runs
            .push(make_non_interactive_run(42, "non-int-2"));

        crate::runner::request_run_label_interactive_input_for_test("non-int-2");

        let mut app = idle_app(state);
        app.current_run_id = Some(42);
        // Operator manually opened the split for this non-interactive run.
        app.split_target = Some(super::split::SplitTarget::Run(42));

        app.synchronize_split_target();

        assert_eq!(
            app.split_target,
            Some(super::split::SplitTarget::Run(42)),
            "manually opened split for a non-interactive run must remain open"
        );
        assert!(
            !app.input_mode,
            "non-interactive run must not gain forced input focus from sync"
        );
    });
}

#[test]
fn poll_agent_run_does_not_close_split_for_non_interactive_run_on_exit() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-interactive-exit-keep-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state
            .agent_runs
            .push(make_non_interactive_run(42, "[Planning]"));

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(42);
        app.run_launched = true;
        // Operator opened the split manually; lifecycle exit must not close it.
        app.split_target = Some(super::split::SplitTarget::Run(42));
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        assert_eq!(
            app.split_target,
            Some(super::split::SplitTarget::Run(42)),
            "non-interactive exit must not auto-close a manually opened split"
        );
    });
}

#[test]
fn skip_to_impl_round_entry_writes_review_scope() {
    with_temp_root(|| {
        let session_id = "skip-to-impl-review-scope";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        // Synthetic-artifacts generation expects spec.md and tasks.toml.
        std::fs::write(artifacts.join("spec.md"), "# spec\n").expect("spec");
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"Task 1\"\ndescription = \"d\"\ntest = \"cargo test\"\nestimated_tokens = 100\n",
        )
        .expect("tasks");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("small change".to_string());
        state.skip_to_impl_kind = Some(crate::artifacts::SkipToImplKind::SkipToImpl);

        let mut app = idle_app(state);
        app.accept_skip_to_implementation()
            .expect("skip-to-impl accept");

        // Round-entry hook in `transition_to_phase` must produce
        // `review_scope.toml` even though no reviewer ever runs on this path.
        let scope_path = session_dir
            .join("rounds")
            .join("001")
            .join("review_scope.toml");
        assert!(
            scope_path.exists(),
            "skip-to-impl entry into ImplementationRound(1) must pin review_scope.toml so the simplifier has a base SHA",
        );
        assert_eq!(app.state.current_phase, Phase::ImplementationRound(1));
    });
}

#[test]
fn final_validation_goal_gap_round_entry_writes_review_scope() {
    with_temp_root(|| {
        let session_id = "goal-gap-review-scope";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");

        let mut state = SessionState::new(session_id.to_string());
        // The state graph allows FinalValidation(R) -> ImplementationRound(R+1)
        // for goal-gap reruns; jumping directly there exercises the round-entry
        // hook in transition_to_phase.
        state.current_phase = Phase::FinalValidation(2);
        state.validation_attempts = 2;
        let mut app = idle_app(state);

        app.transition_to_phase(Phase::ImplementationRound(3))
            .expect("goal-gap rerun transition");

        let scope_path = session_dir
            .join("rounds")
            .join("003")
            .join("review_scope.toml");
        assert!(
            scope_path.exists(),
            "goal-gap rerun entry into ImplementationRound(R+1) must pin review_scope.toml for the next simplifier pass",
        );
    });
}

#[test]
fn impl_round_entry_preserves_existing_review_scope() {
    with_temp_root(|| {
        let session_id = "impl-round-scope-idempotent";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        // Pin the file with a sentinel SHA before transitioning so we can
        // confirm the round-entry hook is idempotent on resume.
        std::fs::write(
            round_dir.join("review_scope.toml"),
            "base_sha = \"already-pinned\"\n",
        )
        .expect("seed scope");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ShardingRunning;
        let mut app = idle_app(state);

        app.transition_to_phase(Phase::ImplementationRound(1))
            .expect("sharding -> impl transition");

        let contents =
            std::fs::read_to_string(round_dir.join("review_scope.toml")).expect("read scope");
        assert!(
            contents.contains("already-pinned"),
            "round entry must not overwrite an existing review_scope.toml; got {contents:?}",
        );
    });
}

#[test]
fn watchdog_warning_emits_summarywarn_and_verbatim_prompt_interrupt() {
    with_temp_root(|| {
        let session_id = "watchdog-warn-ac1-ac7";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(10, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");

        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_NORMAL);

        app.tick_watchdog();

        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(
            inputs.len(),
            1,
            "AC1: exactly one watchdog interrupt should have been queued",
        );
        let (kind, body) = &inputs[0];
        assert_eq!(
            *kind, "interrupt",
            "AC1: warning must use AcpInput::Interrupt"
        );
        assert!(
            body.contains("Liveness warning from codexize watchdog"),
            "AC1: warning preamble missing"
        );
        assert!(
            body.contains("ORIGINAL PROMPT"),
            "AC7: warning body must contain ORIGINAL PROMPT marker"
        );
        assert!(
            body.contains(WATCHDOG_TEST_PROMPT_BODY),
            "AC7: warning body must contain the verbatim prompt text"
        );
        assert!(
            body.contains("10 minutes"),
            "AC1: remaining-minutes count must read from unscaled spec constants"
        );

        let summary_warn_count = app
            .messages
            .iter()
            .filter(|m| {
                m.run_id == run.id
                    && m.kind == MessageKind::SummaryWarn
                    && m.text.contains("watchdog warning")
            })
            .count();
        assert_eq!(
            summary_warn_count, 1,
            "AC1: exactly one SummaryWarn for the warning",
        );

        // Idempotent: a second tick at the same elapsed must not re-send.
        app.tick_watchdog();
        let inputs_after = crate::runner::drain_test_input_receiver_for(&window_name);
        assert!(
            inputs_after.is_empty(),
            "AC1: warning must not re-arm; got {inputs_after:?}",
        );
        let summary_warn_count_after = app
            .messages
            .iter()
            .filter(|m| m.kind == MessageKind::SummaryWarn && m.text.contains("watchdog warning"))
            .count();
        assert_eq!(
            summary_warn_count_after, 1,
            "AC1: SummaryWarn must not duplicate"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_kill_sends_terminate_and_drops_state() {
    with_temp_root(|| {
        let session_id = "watchdog-kill-ac2-partial";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(20, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");

        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );
        // Push elapsed past the kill threshold without ever crossing warn —
        // mirrors a starved poll loop (spec §3.3) so AC2's "kill without prior
        // warning" branch is exercised.
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_NORMAL);

        app.tick_watchdog();

        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(cancels, vec!["terminate"], "AC2: kill must send Terminate");
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert!(
            inputs.is_empty(),
            "AC2: kill path must not also enqueue a warning interrupt"
        );
        let kill_summary = app
            .messages
            .iter()
            .filter(|m| {
                m.run_id == run.id
                    && m.kind == MessageKind::SummaryWarn
                    && m.text.contains("watchdog kill")
            })
            .count();
        assert_eq!(kill_summary, 1, "AC2: exactly one kill SummaryWarn");
        assert!(
            app.watchdog.get(run.id).is_none(),
            "AC2: kill must drop the per-run watchdog state",
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_kill_finalizes_failed_run_and_relaunches_with_different_vendor() {
    with_temp_root(|| {
        let session_id = "watchdog-kill-ac2-failover";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = coder_round_state(session_id);
        let run = make_coder_run(30, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 1, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
        ];
        // The retry attempt #2 will go through the test-launch harness; let
        // it succeed so the relaunch sticks and we can assert the vendor.
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_NORMAL);

        app.tick_watchdog();

        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(cancels, vec!["terminate"], "AC2: Terminate on cancel_tx");

        // Simulate the runner thread reacting to Terminate: the active map
        // entry is dropped and a finish stamp lands with exit_code 143.
        crate::runner::cancel_run_labels_matching(&window_name);
        let stamp = crate::runner::FinishStamp {
            finished_at: chrono::Utc::now().to_rfc3339(),
            exit_code: 143,
            head_before: "base123".to_string(),
            head_after: "base123".to_string(),
            head_state: "stable".to_string(),
            signal_received: "TERM".to_string(),
            working_tree_clean: true,
        };
        let stamp_path = app.finish_stamp_path_for(&run);
        std::fs::create_dir_all(stamp_path.parent().unwrap()).expect("stamp dir");
        crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write stamp");
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        let failed = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == run.id)
            .expect("original run record");
        assert_eq!(failed.status, RunStatus::Failed, "AC2: original run failed");

        let retry = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.stage == "coder" && r.attempt == 2)
            .expect("AC2: vendor failover must launch attempt 2 on a different vendor");
        assert_ne!(
            retry.vendor, run.vendor,
            "AC2: retry vendor must differ from the watchdog-killed vendor"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_uses_tough_thresholds() {
    with_temp_root(|| {
        let session_id = "watchdog-tough-ac3";
        let mut state = coder_round_state(session_id);
        let mut run = make_coder_run(40, 1, 1);
        run.effort = EffortLevel::Tough;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Tough,
        );

        // Past normal warn (10m) but below tough warn (15m): must not fire.
        fast_forward_idle(
            &mut app,
            run.id,
            super::watchdog::WARN_AFTER_NORMAL + Duration::from_secs(60),
        );
        app.tick_watchdog();
        assert!(
            crate::runner::drain_test_input_receiver_for(&window_name).is_empty(),
            "AC3: tough run must not warn at the normal-effort 10 min threshold",
        );

        // Cross the tough warn threshold (15m).
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_TOUGH);
        app.tick_watchdog();
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(
            inputs.len(),
            1,
            "AC3: warning must fire after the tough warn threshold"
        );
        assert!(
            inputs[0].1.contains("15 minutes"),
            "AC3: remaining-minutes must reflect the tough kill-warn gap (30 - 15)"
        );

        // Cross the tough kill threshold (30m).
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_TOUGH);
        app.tick_watchdog();
        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(
            cancels,
            vec!["terminate"],
            "AC3: kill must fire after the tough kill threshold"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn modal_kind_has_final_validation_blocked_variant() {
    use crate::app_runtime::view::ModalKind;
    let kind = ModalKind::FinalValidationBlocked;
    assert_eq!(format!("{kind:?}"), "FinalValidationBlocked");
}

#[test]
fn active_modal_surfaces_final_validation_blocked() {
    use crate::state::{BlockOrigin, Phase};
    with_temp_root(|| {
        let mut state = SessionState::new("active-modal-fv-blocked".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::FinalValidation);
        let app = mk_app(state);
        assert_eq!(app.active_modal(), Some(ModalKind::FinalValidationBlocked));
    });
}

#[test]
fn active_modal_does_not_surface_for_simplification_block() {
    use crate::state::{BlockOrigin, Phase};
    with_temp_root(|| {
        let mut state = SessionState::new("active-modal-simplification-block".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::Simplification);
        let app = mk_app(state);
        assert_eq!(app.active_modal(), None);
    });
}

#[test]
fn active_modal_persists_across_serialization_roundtrip() {
    use crate::state::{BlockOrigin, Phase, SessionState};
    with_temp_root(|| {
        let mut state = SessionState::new("active-modal-fv-roundtrip".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::FinalValidation);
        let serialized = toml::to_string(&state).expect("serialize");
        let deserialized: SessionState = toml::from_str(&serialized).expect("deserialize");
        let resumed = mk_app(deserialized);
        assert_eq!(resumed.active_modal(), Some(ModalKind::FinalValidationBlocked));
    });
}
