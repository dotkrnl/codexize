use crate::app::ModalKind;
use crate::app::test_support::{mk_app, with_temp_root};
use crate::app::{TestLaunchHarness, TestLaunchOutcome};
use crate::logic::selection::{
    CachedModel, Candidate, CliKind, IpbrStageScores, ScoreSource, SubscriptionKind,
};
use crate::state::{
    BlockOrigin, LaunchModes, Message, MessageKind, MessageSender, RunRecord, RunStatus,
    SessionState, Stage,
};
use crossterm::event::KeyCode;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

fn running_recovery_run(id: u64) -> RunRecord {
    RunRecord {
        id,
        stage: "recovery".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "kimi-k2.6".to_string(),
        subscription_label: "moonshotai".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::data::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

fn running_noninteractive_run(id: u64) -> RunRecord {
    let mut run = running_recovery_run(id);
    run.modes.interactive = false;
    run
}

fn running_interactive_run(id: u64) -> RunRecord {
    let mut run = running_recovery_run(id);
    run.modes.interactive = true;
    run
}

fn finished_run(id: u64) -> RunRecord {
    let mut run = running_recovery_run(id);
    run.status = RunStatus::Done;
    run
}

fn cached_start_model() -> CachedModel {
    let candidate = Candidate {
        subscription: SubscriptionKind::Codex,
        cli: CliKind::Codex,
        launch_name: "test-start-model".to_string(),
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free: false,
        official: true,
        quota_disabled: false,
        cheap_eligible: true,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    };
    CachedModel {
        subscription: SubscriptionKind::Codex,
        name: "test-start-model".to_string(),
        ipbr_stage_scores: IpbrStageScores {
            idea: Some(80.0),
            planning: Some(80.0),
            build: Some(80.0),
            review: Some(80.0),
        },
        score_source: ScoreSource::Ipbr,
        candidates: vec![candidate],
        selected_candidate: Some(0),
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 0,
    }
}

fn app_with_stage(stage: Stage) -> crate::app::App {
    let mut state = SessionState::new(format!("cancel-palette-{stage:?}"));
    state.current_stage = stage;
    mk_app(state)
}

#[test]
fn exit_interactive_run_pushes_fsm_into_stopping_go_idle() {
    // Operator pressing /exit during interactive recovery should mark the run
    // as user-stopped so the post-finalisation failure path skips auto-retry.
    // Without this, an empty/invalid recovery.toml at /exit time triggers
    // `maybe_auto_retry`, silently relaunching the agent — exactly the
    // "recovery with finish does not stop" symptom the operator reported.
    // 5c-C reads the stop intent from the FSM rather than the persisted
    // pending_termination mirror; assert on the FSM transition.
    let mut state = SessionState::new("interactive-test".to_string());
    state.current_stage = Stage::Implementation(1);
    state.agent_runs.push(running_recovery_run(7));
    let mut app = mk_app(state);
    app.current_run_id = Some(7);

    app.exit_interactive_run_locally();

    assert!(
        matches!(
            app.fsm.view(),
            crate::lifecycle::AgentState::Stopping {
                after: crate::lifecycle::AfterStop::GoIdle,
                ..
            }
        ),
        "/exit must push the FSM into Stopping(GoIdle); got {:?}",
        app.fsm.view()
    );
}

#[test]
fn exit_interactive_run_without_active_run_is_a_noop() {
    let state = SessionState::new("interactive-test".to_string());
    let mut app = mk_app(state);

    app.exit_interactive_run_locally();

    assert!(matches!(app.fsm.view(), crate::lifecycle::AgentState::Idle));
}

#[test]
fn exit_interactive_run_skips_when_run_not_in_session() {
    // current_run_id points at an id with no matching RunRecord — exit should
    // bail out before transitioning the FSM.
    let state = SessionState::new("interactive-test".to_string());
    let mut app = mk_app(state);
    app.current_run_id = Some(99);

    app.exit_interactive_run_locally();

    assert!(matches!(app.fsm.view(), crate::lifecycle::AgentState::Idle));
}

#[test]
fn palette_interrupt_registered_for_noninteractive_running_agent() {
    let mut state = SessionState::new("interrupt-palette-test".to_string());
    state.agent_runs.push(running_noninteractive_run(10));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must appear in palette when a non-interactive agent is running"
    );
}

#[test]
fn palette_interrupt_registered_for_interactive_running_agent() {
    let mut state = SessionState::new("interrupt-palette-test".to_string());
    state.agent_runs.push(running_interactive_run(11));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must appear in palette when an interactive agent is running"
    );
}

#[test]
fn palette_interrupt_absent_when_no_agent_running() {
    let mut state = SessionState::new("interrupt-palette-test".to_string());
    state.agent_runs.push(finished_run(12));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        !commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must not appear in palette when no agent is running"
    );
}

#[test]
fn palette_interrupt_absent_with_empty_runs() {
    let state = SessionState::new("interrupt-palette-test".to_string());
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        !commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must not appear in palette when there are no runs"
    );
}

#[test]
fn palette_start_registered_for_idle_startable_stage() {
    let mut state = SessionState::new("start-palette-test".to_string());
    state.current_stage = Stage::BrainstormRunning;
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        commands.iter().any(|cmd| cmd.name == "start"),
        ":start must appear when a startable stage has no running agent"
    );
}

#[test]
fn palette_start_hidden_while_agent_running() {
    let mut state = SessionState::new("start-palette-test".to_string());
    state.current_stage = Stage::Implementation(1);
    state.agent_runs.push(running_noninteractive_run(10));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        !commands.iter().any(|cmd| cmd.name == "start"),
        ":start must not appear while an agent is already running"
    );
}

#[test]
fn palette_start_clears_stale_launch_state_and_starts_model_refresh() {
    let mut state = SessionState::new("start-palette-test".to_string());
    state.current_stage = Stage::BrainstormRunning;
    let mut app = mk_app(state);
    app.models.clear();
    app.current_run_id = Some(99);
    app.run_launched = true;

    let should_exit = app.execute_palette_command("start", "");

    assert!(!should_exit);
    assert_eq!(app.current_run_id, None);
    assert!(!app.run_launched);
    assert!(
        matches!(
            app.model_refresh,
            crate::app::ModelRefreshState::Fetching { .. }
        ),
        ":start should kick model refresh when no model candidates are loaded"
    );
}

#[test]
fn palette_start_launches_current_stage_when_models_are_available() {
    with_temp_root(|| {
        let mut state = SessionState::new("start-palette-launch-test".to_string());
        state.current_stage = Stage::BrainstormRunning;
        state.idea_text = Some("build a reliable manual start command".to_string());
        let mut app = mk_app(state);
        app.models.push(cached_start_model());
        app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
            outcomes: VecDeque::from([TestLaunchOutcome {
                exit_code: 0,
                artifact_contents: None,
                launch_error: None,
            }]),
        })));

        let should_exit = app.execute_palette_command("start", "");

        assert!(!should_exit);
        assert!(app.run_launched);
        assert_eq!(app.current_run_id, Some(1));
        assert!(
            app.state
                .agent_runs
                .iter()
                .any(|run| run.stage == "brainstorm" && run.status == RunStatus::Running),
            ":start should append a running brainstorm run"
        );
    });
}

#[test]
fn palette_cancel_registered_for_cancellable_session_states() {
    for stage in [
        Stage::IdeaInput,
        Stage::WaitingToImplement,
        Stage::BlockedNeedsUser,
        Stage::RepoStateUpdateRunning,
    ] {
        let app = app_with_stage(stage);
        assert!(
            app.palette_commands()
                .iter()
                .any(|cmd| cmd.name == "cancel"),
            ":cancel must appear for cancellable stage {stage:?}"
        );
    }
}

#[test]
fn palette_cancel_hidden_for_terminal_session_states() {
    for stage in [Stage::Done, Stage::Cancelled] {
        let app = app_with_stage(stage);
        assert!(
            app.palette_commands()
                .iter()
                .all(|cmd| cmd.name != "cancel"),
            ":cancel must be hidden for terminal stage {stage:?}"
        );
    }
}

#[test]
fn palette_cancel_opens_confirmation_modal_and_dismisses_without_transition() {
    let mut app = app_with_stage(Stage::WaitingToImplement);

    app.execute_palette_input("cancel");
    assert_eq!(app.active_modal(), Some(ModalKind::CancelSession));

    assert!(!app.handle_modal_key(
        ModalKind::CancelSession,
        crate::app::test_support::key(KeyCode::Char('n'))
    ));
    assert_eq!(app.active_modal(), None);
    assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
}

#[test]
fn confirmed_idle_cancel_transitions_to_cancelled() {
    crate::app::test_support::with_temp_root(|| {
        let mut app = app_with_stage(Stage::WaitingToImplement);

        app.execute_palette_input("cancel");
        assert!(!app.handle_modal_key(
            ModalKind::CancelSession,
            crate::app::test_support::key(KeyCode::Enter)
        ));

        assert_eq!(app.state.current_stage, Stage::Cancelled);
    });
}

#[test]
fn confirmed_blocked_cancel_transitions_to_cancelled() {
    crate::app::test_support::with_temp_root(|| {
        let mut state = SessionState::new("cancel-blocked".to_string());
        state.current_stage = Stage::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::FinalValidation);
        let mut app = mk_app(state);

        app.execute_palette_input("cancel");
        assert!(!app.handle_modal_key(
            ModalKind::CancelSession,
            crate::app::test_support::key(KeyCode::Enter)
        ));

        assert_eq!(app.state.current_stage, Stage::Cancelled);
        assert_eq!(app.state.block_origin, None);
    });
}

#[test]
fn confirmed_idle_cancel_preserves_messages_and_artifacts() {
    crate::app::test_support::with_temp_root(|| {
        let mut state = SessionState::new("cancel-preserves-audit".to_string());
        state.current_stage = Stage::WaitingToImplement;
        state
            .append_message(&Message {
                ts: chrono::Utc::now(),
                run_id: 0,
                kind: MessageKind::Summary,
                sender: MessageSender::System,
                text: "audit message".to_string(),
            })
            .expect("message append");
        let artifact = crate::state::session_dir(&state.session_id)
            .join("artifacts")
            .join("note.txt");
        std::fs::create_dir_all(artifact.parent().unwrap()).expect("artifact dir");
        std::fs::write(&artifact, "preserved").expect("artifact");
        let mut app = mk_app(state);

        app.execute_palette_input("cancel");
        assert!(!app.handle_modal_key(
            ModalKind::CancelSession,
            crate::app::test_support::key(KeyCode::Enter)
        ));

        assert_eq!(app.state.current_stage, Stage::Cancelled);
        assert_eq!(std::fs::read_to_string(&artifact).unwrap(), "preserved");
        let messages = SessionState::load_messages(&app.state.session_id).expect("messages");
        assert!(
            messages
                .iter()
                .any(|message| message.text == "audit message")
        );
    });
}

#[test]
fn confirmed_running_cancel_signals_runner_and_finalizes_to_cancelled() {
    crate::app::test_support::with_temp_root(|| {
        let window = "[Cancel Running Test]";
        crate::data::runner::request_run_label_active_for_test(window);
        let mut run = running_noninteractive_run(7101);
        run.window_name = window.to_string();
        let mut state = SessionState::new("cancel-running".to_string());
        state.current_stage = Stage::RepoStateUpdateRunning;
        state.agent_runs.push(run.clone());
        let mut app = mk_app(state);
        app.current_run_id = Some(run.id);

        app.execute_palette_input("cancel");
        assert!(!app.handle_modal_key(
            ModalKind::CancelSession,
            crate::app::test_support::key(KeyCode::Enter)
        ));

        assert_eq!(
            crate::data::runner::drain_test_cancel_receiver_for(window),
            vec!["terminate"]
        );
        assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);

        app.complete_run_finalization(&run, Some("Operator Killed".to_string()))
            .expect("cancel finalization");
        assert_eq!(app.state.current_stage, Stage::Cancelled);
    });
}

#[test]
fn repeated_running_cancel_while_pending_is_idempotent() {
    crate::app::test_support::with_temp_root(|| {
        let window = "[Cancel Idempotent Test]";
        crate::data::runner::request_run_label_active_for_test(window);
        let mut run = running_noninteractive_run(7102);
        run.window_name = window.to_string();
        let mut state = SessionState::new("cancel-running-idempotent".to_string());
        state.current_stage = Stage::RepoStateUpdateRunning;
        state.agent_runs.push(run.clone());
        let mut app = mk_app(state);
        app.current_run_id = Some(run.id);

        app.execute_palette_input("cancel");
        assert!(!app.handle_modal_key(
            ModalKind::CancelSession,
            crate::app::test_support::key(KeyCode::Enter)
        ));
        assert_eq!(
            crate::data::runner::drain_test_cancel_receiver_for(window),
            vec!["terminate"]
        );

        app.execute_palette_input("cancel");

        assert_eq!(
            crate::data::runner::drain_test_cancel_receiver_for(window),
            Vec::<&str>::new()
        );
        assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);
    });
}

#[test]
fn palette_config_registered_and_opens_panel() {
    let state = SessionState::new("config-palette-test".to_string());
    let mut app = mk_app(state);
    let commands = app.palette_commands();
    let config = commands
        .iter()
        .find(|cmd| cmd.name == "config")
        .expect(":config command registered");
    assert_eq!(config.aliases, &["cfg"]);
    assert_eq!(config.key_hint, None);

    app.execute_palette_input("cfg");

    assert!(app.config_panel.is_some());
}

#[test]
fn palette_does_not_advertise_config_reset_command() {
    let state = SessionState::new("config-reset-palette-test".to_string());
    let app = mk_app(state);

    assert!(
        app.palette_commands()
            .iter()
            .all(|command| command.name != "config-reset-section"),
        "config reset should not be a palette command"
    );
}

#[test]
#[serial_test::serial]
fn ctrl_s_in_panel_clears_dirty_and_reloads_arc_config() {
    // Bake a config file that pins paths.sessions_root, swing
    // CODEXIZE_CONFIG at it, mutate the panel's in-memory config to a
    // different path, then drive Ctrl-S through the App's key handler.
    // The Saved outcome must close the panel, clear dirty, and refresh
    // both the Arc<Config> and the cached PathsView.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let new_root = dir.path().join("alt-sessions");
    std::fs::write(
        &path,
        b"[meta]\nversion = 1\n[paths]\nsessions_root = \"/tmp/initial\"\n",
    )
    .unwrap();
    let prev = std::env::var_os("CODEXIZE_CONFIG");
    // SAFETY: serialized via #[serial]; restored unconditionally below.
    unsafe {
        std::env::set_var("CODEXIZE_CONFIG", &path);
    }

    let state = SessionState::new("ctrl-s-reload".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config");
    let panel = app.config_panel.as_mut().expect("panel open");
    crate::data::config::mutate::set_value(
        &mut panel.config,
        "paths.sessions_root",
        new_root.to_str().unwrap(),
    )
    .unwrap();
    panel.dirty = true;

    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    assert!(
        app.config_panel.is_none(),
        "Saved outcome must close the App's config panel"
    );
    assert_eq!(
        app.paths.sessions_root, new_root,
        "Arc<Config>-derived PathsView must reflect the freshly-saved file"
    );
    assert_eq!(
        app.config.paths.sessions_root.value().as_str(),
        new_root.to_str().unwrap(),
        "underlying Arc<Config> must be replaced after save"
    );

    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEXIZE_CONFIG", v),
            None => std::env::remove_var("CODEXIZE_CONFIG"),
        }
    }
}

#[test]
#[serial_test::serial]
fn ctrl_s_with_invalid_inline_buffer_aborts_save_and_keeps_edit() {
    // An inline-edit buffer that fails validation must abort the save,
    // leave the operator in edit mode, and not clear dirty so the
    // pending change isn't silently dropped.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, b"[meta]\nversion = 1\n").unwrap();
    let prev = std::env::var_os("CODEXIZE_CONFIG");
    // SAFETY: serialized via #[serial]; restored unconditionally below.
    unsafe {
        std::env::set_var("CODEXIZE_CONFIG", &path);
    }

    let state = SessionState::new("ctrl-s-invalid".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config");
    let panel = app.config_panel.as_mut().expect("panel open");
    // Focus retry_attempts (Integer field with min=1) and stage an edit
    // that violates the minimum.
    let field_idx =
        crate::app_runtime::views::config_panel::field_index_for_test("ntfy.retry_attempts");
    panel.set_focus_for_test(field_idx);
    panel.dirty = true;
    panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    panel.set_edit_buffer_for_test("0".to_string());

    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    let panel = app
        .config_panel
        .as_ref()
        .expect("panel must remain open on save abort");
    assert!(panel.editing.is_some(), "edit mode must persist on abort");
    assert!(panel.dirty, "dirty must not be cleared by an aborted save");

    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEXIZE_CONFIG", v),
            None => std::env::remove_var("CODEXIZE_CONFIG"),
        }
    }
}

#[test]
fn palette_config_with_exact_section_arg_jumps_to_section() {
    let state = SessionState::new("config-jump".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config ntfy");
    let panel = app.config_panel.as_ref().expect("panel open");
    assert_eq!(panel.current_section_name(), "notifications");
}

#[test]
fn palette_config_with_unique_prefix_jumps_to_section() {
    let state = SessionState::new("config-prefix".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config acp.po");
    let panel = app.config_panel.as_ref().expect("panel open");
    assert_eq!(panel.current_section_name(), "agents");
}

#[test]
fn palette_config_with_unknown_section_arg_errors_and_does_not_open() {
    let state = SessionState::new("config-unknown".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config nope");
    assert!(app.config_panel.is_none());
    let status = app
        .status_line
        .borrow()
        .render()
        .expect("status line")
        .to_string();
    assert!(status.to_lowercase().contains("unknown"));
}

#[test]
fn config_panel_remembers_last_section_within_app() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let state = SessionState::new("config-memory".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config acp.policy");
    // Close via Esc -> should record the friendly Agents page.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.last_config_section.as_deref(), Some("agents"));

    app.execute_palette_input("config");
    let panel = app.config_panel.as_ref().expect("panel reopens");
    assert_eq!(
        panel.current_section_name(),
        "agents",
        "no-arg :config must restore the last-viewed section in the same App"
    );
}

#[test]
fn palette_config_refuses_too_narrow_terminal() {
    let state = SessionState::new("config-palette-test".to_string());
    let mut app = mk_app(state);
    app.body_inner_width = 49;

    app.execute_palette_input("config");

    assert!(app.config_panel.is_none());
    let status = app
        .status_line
        .borrow()
        .render()
        .expect("status line")
        .to_string();
    assert!(status.contains(crate::app::config_panel::terminal_too_narrow_message()));
}

#[test]
fn bridged_tab_keeps_config_panel_open_and_switches_page() {
    let state = SessionState::new("config-tab-bridge".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config");
    assert_eq!(
        app.config_panel
            .as_ref()
            .expect("panel open")
            .current_section_name(),
        "general"
    );

    let quit = app.handle_app_command(crate::app_runtime::AppCommand::Session(
        std::sync::Arc::from(app.state.session_id.clone()),
        crate::app_runtime::SessionCommand::ConfigPanel(
            crate::app_runtime::ConfigPanelCommand::NextSection,
        ),
    ));

    assert!(!quit, "tab inside config panel must not quit the app");
    assert_eq!(
        app.config_panel
            .as_ref()
            .expect("panel still open")
            .current_section_name(),
        "models"
    );
}

#[test]
fn cancel_on_done_session_is_noop() {
    // AC-7 / spec §"Cancellation": on Done or Cancelled, :cancel is a no-op
    // (hidden from the palette or surfaces a non-error toast). Verify that
    // even if the command were somehow invoked, the session state does not
    // change.
    crate::app::test_support::with_temp_root(|| {
        let mut state = SessionState::new("cancel-done-noop".to_string());
        state.current_stage = Stage::Done;
        let mut app = mk_app(state);

        // :cancel is hidden from the palette for Done sessions.
        assert!(
            app.palette_commands()
                .iter()
                .all(|cmd| cmd.name != "cancel"),
            ":cancel must be hidden for Done session"
        );

        // Even if invoked directly, the stage must not change.
        app.execute_palette_input("cancel");
        assert_eq!(app.state.current_stage, Stage::Done);
        assert_eq!(app.active_modal(), None);
    });
}
