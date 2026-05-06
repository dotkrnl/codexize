use super::*;

// The watchdog now measures wall-clock idle since the last live-summary
// write — there is no tool-call exclusion to test (see chunk_06's prior
// `watchdog_clock_pauses_during_tool_call_activity` removal). The other
// AC tests in this file still exercise warn/kill thresholds, no-rearm,
// degraded prompt fallback, and clock compression.

#[test]
fn watchdog_does_not_arm_for_interactive_runs() {
    with_temp_root(|| {
        let session_id = "watchdog-interactive-ac5";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut interactive = make_planning_run(60, 1);
        interactive.modes.interactive = true;
        let window_name = interactive.window_name.clone();
        state.agent_runs.push(interactive.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(interactive.id);
        app.run_launched = true;
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));

        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let run_id = app.state.next_agent_run_id();

        // Drive `start_run_tracking` for an interactive launch and assert
        // the registry stays empty (AC5). The path mirrors the brainstorm
        // launch — start_run_tracking is the only non-test entry point that
        // registers the watchdog.
        app.start_run_tracking(
            run_id,
            "planning",
            None,
            1,
            "gpt-5".to_string(),
            "codex".to_string(),
            window_name.clone(),
            EffortLevel::Normal,
            crate::state::LaunchModes {
                yolo: false,
                cheap: false,
                interactive: true,
            },
            std::path::PathBuf::from("prompts/planning.md"),
        );
        assert!(
            app.watchdog.is_empty(),
            "AC5: interactive run must not register watchdog state"
        );

        // Even with a long-stale fake heartbeat, tick_watchdog is a no-op
        // because nothing is registered.
        app.tick_watchdog();
        assert!(
            crate::runner::drain_test_input_receiver_for(&window_name).is_empty(),
            "AC5: no warning must be sent for interactive runs"
        );
        assert!(
            crate::runner::drain_test_cancel_receiver_for(&window_name).is_empty(),
            "AC5: no Terminate must be sent for interactive runs"
        );
        let any_summary_warn = app
            .messages
            .iter()
            .any(|m| m.kind == MessageKind::SummaryWarn);
        assert!(
            !any_summary_warn,
            "AC5: no SummaryWarn must be appended for interactive runs",
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_warning_does_not_re_arm_after_summary_recovery() {
    with_temp_root(|| {
        let session_id = "watchdog-no-rearm-ac6";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(70, 1, 1);
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

        // Stage 1: cross warn — exactly one warning fires.
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_NORMAL);
        app.tick_watchdog();
        assert_eq!(
            crate::runner::drain_test_input_receiver_for(&window_name).len(),
            1,
            "AC6: first warning must fire"
        );

        // Stage 2: the agent writes one summary — clock resets, but the
        // `warned` flag stays true (operator answer 5: no re-arm).
        if let Some(s) = app.watchdog.get_mut(run.id) {
            s.on_live_summary_event(tokio::time::Instant::now());
            assert!(
                s.warned,
                "AC6: warned flag must persist across summary writes"
            );
        }

        // Stage 3: stall again past the kill threshold. Kill fires; no second
        // warning.
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_NORMAL);
        app.tick_watchdog();
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert!(
            inputs.is_empty(),
            "AC6: no second warning must be sent after recovery; got {inputs:?}",
        );
        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(
            cancels,
            vec!["terminate"],
            "AC6: kill must still fire on the second stall"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_warning_falls_back_when_prompt_cannot_be_read() {
    with_temp_root(|| {
        let session_id = "watchdog-degraded-fallback";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(80, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        // Point at a prompt path that does not exist on disk.
        let missing_path = session_state::session_dir(session_id)
            .join("prompts")
            .join("does-not-exist.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            missing_path,
            EffortLevel::Normal,
        );
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_NORMAL);

        app.tick_watchdog();

        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(inputs.len(), 1, "warning must still fire on read failure");
        assert!(
            inputs[0]
                .1
                .contains(super::watchdog::PROMPT_UNAVAILABLE_BODY),
            "fallback body must use the documented degraded message",
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[tokio::test(start_paused = true)]
async fn watchdog_register_uses_paused_tokio_time() {
    let mut registry = super::watchdog::WatchdogRegistry::from_env();
    let now = tokio::time::Instant::now();
    registry.register(
        1,
        EffortLevel::Normal,
        "[paused]".to_string(),
        std::path::PathBuf::from("/p"),
        now,
    );
    let state = registry.get(1).expect("registered");
    assert_eq!(state.warn_threshold, super::watchdog::WARN_AFTER_NORMAL);
    assert_eq!(state.kill_threshold, super::watchdog::KILL_AFTER_NORMAL);
    assert_eq!(state.warning_remaining_minutes, 10);
    tokio::time::advance(super::watchdog::WARN_AFTER_NORMAL).await;
    assert_eq!(
        registry.evaluate_all(tokio::time::Instant::now())[0].1,
        super::watchdog::WatchdogDecision::EmitWarning
    );
}
