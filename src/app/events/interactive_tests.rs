use crate::app::TerminationIntent;
use crate::app::test_support::mk_app;
use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};

fn running_recovery_run(id: u64) -> RunRecord {
    RunRecord {
        id,
        stage: "recovery".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "kimi-k2.6".to_string(),
        vendor: "moonshotai".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
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

#[test]
fn exit_interactive_run_marks_pending_termination_stop_only() {
    // Operator pressing /exit during interactive recovery should mark the run
    // as user-stopped so the post-finalisation failure path skips auto-retry.
    // Without this, an empty/invalid recovery.toml at /exit time triggers
    // `maybe_auto_retry`, silently relaunching the agent — exactly the
    // "recovery with finish does not stop" symptom the operator reported.
    let mut state = SessionState::new("interactive-test".to_string());
    state.current_phase = crate::state::Phase::BuilderRecovery(1);
    state.agent_runs.push(running_recovery_run(7));
    let mut app = mk_app(state);
    app.current_run_id = Some(7);

    app.exit_interactive_run_locally();

    let pending = app
        .pending_termination
        .as_ref()
        .expect("/exit must mark pending termination");
    assert_eq!(pending.run_id, 7);
    assert_eq!(pending.intent, TerminationIntent::StopOnly);
}

#[test]
fn exit_interactive_run_without_active_run_is_a_noop() {
    let state = SessionState::new("interactive-test".to_string());
    let mut app = mk_app(state);

    app.exit_interactive_run_locally();

    assert!(app.pending_termination.is_none());
}

#[test]
fn exit_interactive_run_skips_when_run_not_in_session() {
    // current_run_id points at an id with no matching RunRecord — exit should
    // bail out before queueing pending_termination.
    let state = SessionState::new("interactive-test".to_string());
    let mut app = mk_app(state);
    app.current_run_id = Some(99);

    app.exit_interactive_run_locally();

    assert!(app.pending_termination.is_none());
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
    let field_idx = crate::ui::config_panel::field_index_for_test("ntfy.retry_attempts");
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
    assert_eq!(panel.current_section_name(), "ntfy");
}

#[test]
fn palette_config_with_unique_prefix_jumps_to_section() {
    let state = SessionState::new("config-prefix".to_string());
    let mut app = mk_app(state);
    app.execute_palette_input("config acp.po");
    let panel = app.config_panel.as_ref().expect("panel open");
    assert_eq!(panel.current_section_name(), "acp.policy");
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
    // Close via Esc → should record acp.policy as the last-viewed section.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.last_config_section.as_deref(), Some("acp.policy"));

    app.execute_palette_input("config");
    let panel = app.config_panel.as_ref().expect("panel reopens");
    assert_eq!(
        panel.current_section_name(),
        "acp.policy",
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
    assert!(status.contains(crate::ui::config_panel::terminal_too_narrow_message()));
}
