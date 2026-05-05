use super::*;

#[test]
fn skip_modal_decline_enters_spec_review() {
    with_temp_root(|| {
        let session_id = "skip-decline";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("rationale".to_string());

        let mut app = idle_app(state);
        app.decline_skip_to_implementation()
            .expect("decline should succeed");

        assert_eq!(app.state.current_phase, Phase::SpecReviewRunning);
        assert!(app.state.skip_to_impl_rationale.is_none());
    });
}

#[test]
fn skip_modal_accept_generates_artifacts_and_enters_impl_round_one() {
    with_temp_root(|| {
        let session_id = "skip-accept";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("trivial".to_string());

        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("mk artifacts dir");
        std::fs::write(artifacts.join("spec.md"), "# Spec\n\nA trivial feature.\n")
            .expect("write spec");

        let mut app = idle_app(state);
        app.accept_skip_to_implementation()
            .expect("accept should succeed");

        assert_eq!(app.state.current_phase, Phase::ImplementationRound(1));
        assert!(artifacts.join("plan.md").exists());
        assert!(artifacts.join("tasks.toml").exists());
        assert!(!artifacts.join("implementation.json").exists());
        assert_eq!(app.state.builder.pending, vec![1]);
        assert!(app.state.builder.current_task.is_none());
    });
}

#[test]
fn skip_modal_accept_nothing_to_do_bypasses_final_validation_and_finishes() {
    with_temp_root(|| {
        let session_id = "skip-accept-nothing-to-do";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("already complete".to_string());
        state.skip_to_impl_kind = Some(crate::artifacts::SkipToImplKind::NothingToDo);

        let mut app = idle_app(state);
        app.accept_skip_to_implementation()
            .expect("accept should succeed");

        assert_eq!(app.state.current_phase, Phase::Done);
        assert_eq!(app.state.validation_attempts, 0);
    });
}

#[test]
fn enter_builder_recovery_sets_interactive_for_human_blocked() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-interactive".to_string());
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
        let session_dir = session_state::session_dir("recovery-interactive");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut app = idle_app(state);
        app.enter_builder_recovery(1, Some(1), Some("needs human".to_string()), "human_blocked");

        // The recovery pipeline item should be interactive=true for human_blocked
        let recovery_items: Vec<_> = app
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|i| i.stage == "recovery")
            .collect();
        assert_eq!(recovery_items.len(), 1);
        assert_eq!(recovery_items[0].interactive, Some(true));
        assert_eq!(recovery_items[0].trigger.as_deref(), Some("human_blocked"));
        assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
    });
}

#[test]
fn enter_builder_recovery_sets_non_interactive_for_agent_pivot() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-non-interactive".to_string());
        state.current_phase = Phase::ReviewRound(2);
        let session_dir = session_state::session_dir("recovery-non-interactive");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 2\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut app = idle_app(state);
        app.enter_builder_recovery(2, None, None, "agent_pivot");

        let recovery_items: Vec<_> = app
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|i| i.stage == "recovery")
            .collect();
        assert_eq!(recovery_items.len(), 1);
        assert_eq!(recovery_items[0].interactive, Some(false));
        assert_eq!(recovery_items[0].trigger.as_deref(), Some("agent_pivot"));
    });
}

#[test]
fn pending_guard_reset_finalizes_as_forbidden_head_advance() {
    with_temp_root(|| {
        let session_id = "pending-guard-reset";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        let run = make_brainstorm_run(10);
        state.agent_runs.push(run.clone());
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 10,
            captured_head: "abc123".to_string(),
            current_head: "def456".to_string(),
            warnings: vec!["some guard warning".to_string()],
        });
        let mut app = mk_app(state);

        app.accept_guard_reset().expect("accept_guard_reset ok");

        assert!(
            app.state.pending_guard_decision.is_none(),
            "pending_guard_decision must be cleared after reset"
        );
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 10)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Failed);
        assert_eq!(
            finalized.error.as_deref(),
            Some("forbidden_head_advance"),
            "run error must be forbidden_head_advance"
        );
        let warned = app
            .messages
            .iter()
            .any(|m| m.kind == MessageKind::SummaryWarn && m.text.contains("some guard warning"));
        assert!(warned, "guard warning must be replayed as SummaryWarn");
    });
}

#[test]
fn pending_guard_keep_preserves_normal_semantics() {
    with_temp_root(|| {
        let session_id = "pending-guard-keep";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        let run = make_brainstorm_run(20);
        state.agent_runs.push(run.clone());
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 20,
            captured_head: "abc123".to_string(),
            current_head: "def456".to_string(),
            warnings: vec!["kept-warning".to_string()],
        });
        let mut app = mk_app(state);
        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
            .expect("write spec");

        app.accept_guard_keep().expect("accept_guard_keep ok");

        assert!(
            app.state.pending_guard_decision.is_none(),
            "pending_guard_decision must be cleared after keep"
        );
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 20)
            .expect("run");
        assert_eq!(
            finalized.status,
            RunStatus::Done,
            "run must succeed on keep"
        );
        let kept_warn = app.messages.iter().any(|m| {
            m.kind == MessageKind::SummaryWarn
                && m.text.contains("operator kept unauthorized commit")
        });
        assert!(kept_warn, "operator-kept warning must be emitted");
        assert_ne!(
            app.state.current_phase,
            Phase::GitGuardPending,
            "phase must advance after keep"
        );
    });
}

#[test]
fn pending_guard_modal_reset_key_dispatches_to_reset() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-reset", 30));

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert!(app.state.pending_guard_decision.is_none());
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 30)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Failed);
        assert_eq!(finalized.error.as_deref(), Some("forbidden_head_advance"));
    });
}

#[test]
fn pending_guard_modal_keep_key_dispatches_to_keep() {
    with_temp_root(|| {
        let session_id = "pending-guard-key-keep";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
            .expect("write spec");
        let mut app = mk_app(make_pending_guard_state(session_id, 31));

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('K')));

        assert!(!should_quit);
        assert!(app.state.pending_guard_decision.is_none());
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 31)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Done);
        assert_ne!(app.state.current_phase, Phase::GitGuardPending);
    });
}

#[test]
fn palette_texts_command_toggles_persisted_noninteractive_text_visibility() {
    with_temp_root(|| {
        let session_id = "palette-texts-toggle";
        let state = SessionState::new(session_id.to_string());
        state.save().expect("save initial state");
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "text".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(app.state.show_noninteractive_texts);
        let saved = SessionState::load(session_id).expect("load saved state");
        assert!(saved.show_noninteractive_texts);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "messages".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(!app.state.show_noninteractive_texts);
        let saved = SessionState::load(session_id).expect("load saved state");
        assert!(!saved.show_noninteractive_texts);
    });
}

#[test]
fn palette_verbose_command_toggles_persisted_thinking_visibility() {
    with_temp_root(|| {
        let session_id = "palette-verbose-toggle";
        let state = SessionState::new(session_id.to_string());
        state.save().expect("save initial state");
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "verbose".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(app.state.show_thinking_texts);
        let saved = SessionState::load(session_id).expect("load saved state");
        assert!(saved.show_thinking_texts);
    });
}

#[test]
fn interactive_palette_command_closes_after_execution() {
    with_temp_root(|| {
        let session_id = "interactive-palette-command-close";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        state.save().expect("save initial state");
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "verbose".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(app.state.show_thinking_texts);
        assert!(
            !app.palette.open,
            "executed commands should close the : box"
        );
        assert!(app.palette.buffer.is_empty());
    });
}

#[test]
fn interactive_exit_is_handled_locally_without_quitting_tui() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-exit-local".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "/exit".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.current_run_id, Some(7));
        assert!(!app.input_mode);
        assert!(!app.palette.open);
        assert!(app.input_buffer.is_empty());
    });
}

#[test]
fn agent_exit_suggestion_opens_requests_modal() {
    with_temp_root(|| {
        let (app, _window_name) = app_waiting_on_agent_exit("agent-exit-modal");

        assert_eq!(app.active_modal(), Some(ModalKind::InteractiveExitPrompt));
    });
}

#[test]
fn agent_exit_suggestion_enter_exits_interactive_run() {
    with_temp_root(|| {
        let (mut app, window_name) = app_waiting_on_agent_exit("agent-exit-enter");

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert!(!crate::runner::run_label_is_waiting_for_input(&window_name));
        assert_eq!(app.active_modal(), None);
    });
}

#[test]
fn agent_exit_suggestion_typing_starts_request_input() {
    with_temp_root(|| {
        let (mut app, window_name) = app_waiting_on_agent_exit("agent-exit-type");

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('f')));

        assert!(!should_quit);
        assert!(app.input_mode);
        assert_eq!(app.input_buffer, "f");
        assert!(crate::runner::run_label_is_waiting_for_input(&window_name));
        assert_eq!(app.active_modal(), None);
    });
}

#[test]
fn idea_input_leading_colon_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("idea-leading-colon".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));

        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::Idea)
        );
    });
}

#[test]
fn footer_interactive_leading_colon_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("footer-interactive-leading-colon".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::FooterInteractive)
        );
        assert!(!app.input_mode);
    });
}

#[test]
fn leading_colon_from_paste_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("idea-paste-leading-colon".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);
        app.handle_paste(":cheap");

        assert!(app.palette.open);
        assert_eq!(app.palette.buffer, "cheap");
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::Idea)
        );
    });
}

#[test]
fn edit_derived_leading_colon_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("footer-edit-derived-leading-colon".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char('c')));
        app.handle_key(key(crossterm::event::KeyCode::Char('h')));
        app.handle_key(key(crossterm::event::KeyCode::Char('e')));
        app.handle_key(key(crossterm::event::KeyCode::Char('a')));
        app.handle_key(key(crossterm::event::KeyCode::Char('p')));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Home,
            crossterm::event::KeyModifiers::NONE,
        ));
        app.handle_key(key(crossterm::event::KeyCode::Char(':')));

        assert!(app.palette.open);
        assert_eq!(app.palette.buffer, "cheap");
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::FooterInteractive)
        );
    });
}

#[test]
fn idea_input_treats_q_as_text_before_global_quit() {
    with_temp_root(|| {
        let mut state = SessionState::new("idea-input-q-priority".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('q')));

        assert!(!should_quit, "q should be consumed by the idea input box");
        assert!(app.input_mode, "typing should focus the input box");
        assert_eq!(app.input_buffer, "q");
    });
}

#[test]
fn command_mode_esc_restores_split_interactive_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-command-esc-restore".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);
        app.open_split_target(super::split::SplitTarget::Run(7));
        app.input_mode = true;

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::SplitInteractive)
        );

        app.handle_key(key(crossterm::event::KeyCode::Esc));

        assert!(!app.palette.open);
        assert!(app.input_mode);
    });
}

#[test]
fn command_mode_backspace_on_empty_buffer_restores_footer_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("footer-command-backspace-restore".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());

        app.handle_key(key(crossterm::event::KeyCode::Backspace));

        assert!(!app.palette.open);
        assert!(app.input_mode);
        assert!(app.input_buffer.is_empty());
    });
}

#[test]
fn unknown_command_in_waiting_interactive_mode_is_sent_as_user_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("unknown-command-waiting".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "unknown-cmd".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(
            app.messages
                .iter()
                .any(|m| { m.kind == MessageKind::UserInput && m.text == "unknown-cmd" })
        );
    });
}

#[test]
fn interrupt_command_interrupts_active_interactive_turn_and_echoes_user_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("interrupt-command-active".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        run.window_name = "[Interrupt Active]".to_string();
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_active_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "interrupt please stop and do this instead".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert!(app.messages.iter().any(|message| {
            message.kind == MessageKind::UserInput
                && message.text == "please stop and do this instead"
        }));
        assert!(!crate::runner::run_label_is_waiting_for_input(&window_name));
        crate::runner::shutdown_all_runs();
    });
}

#[test]
fn unknown_command_outside_waiting_mode_sets_status_and_is_not_persisted() {
    with_temp_root(|| {
        let mut state = SessionState::new("unknown-command-not-waiting".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "unknown-cmd".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(
            !app.messages
                .iter()
                .any(|m| m.kind == MessageKind::UserInput)
        );
        let status = app.status_line.borrow().render().expect("status flash");
        assert!(
            status
                .to_string()
                .contains("palette: unknown command \"unknown-cmd\"")
        );
    });
}
