use codexize::app::AppStartupOrigin;
use codexize::app_runtime::{AppCommand, UiKey, UiKeyCode};
use codexize::app_shell::{AppShell, ShellCommandOutcome, ShellEvent, ShellFocus};
use codexize::data::config::Config;
use codexize::state::{Phase, SessionState};
use serial_test::serial;
use std::sync::Arc;

fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let prev = std::env::var_os("CODEXIZE_ROOT");

    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prev {
            Some(value) => std::env::set_var("CODEXIZE_ROOT", value),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    result.expect("test panicked")
}

fn session(id: &str, phase: Phase) -> SessionState {
    let mut state = SessionState::new(id.to_string());
    state.idea_text = Some(format!("idea for {id}"));
    state.current_phase = phase;
    state
}

fn save_session(id: &str, phase: Phase) -> SessionState {
    let state = session(id, phase);
    state.save().expect("save session");
    state
}

fn shell_for(initial: SessionState) -> AppShell {
    AppShell::new(
        initial,
        AppStartupOrigin::Default,
        Arc::new(Config::baked_defaults()),
    )
    .expect("shell")
}

fn key(code: UiKeyCode) -> AppCommand {
    AppCommand::KeyPress(UiKey {
        code,
        ctrl: false,
        alt: false,
    })
}

#[test]
#[serial]
fn shell_starts_with_one_workspace_and_sidebar_hidden() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let shell = shell_for(initial);

        assert_eq!(shell.focused_session_id(), "20260511-090000-000000001");
        assert_eq!(shell.running_session_id(), None);
        assert_eq!(shell.open_workspace_count(), 1);
        assert!(!shell.sidebar_view().visible);
        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .current_run_id(),
            None
        );
    });
}

#[test]
#[serial]
fn sidebar_enter_lazily_opens_and_focuses_without_changing_running_session() {
    with_temp_root(|| {
        let first = save_session("20260511-090000-000000001", Phase::ShardingRunning);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(first);

        shell.apply_event(ShellEvent::RunStarted {
            session_id: "20260511-090000-000000001".into(),
            run_id: 7,
        });
        shell.toggle_sessions_sidebar().expect("toggle");
        shell.focus_sidebar();
        shell
            .select_sidebar_session("20260511-091000-000000001")
            .expect("select");

        assert_eq!(shell.open_workspace_count(), 1);
        shell
            .open_selected_sidebar_session()
            .expect("open selected");

        assert_eq!(shell.focused_session_id(), "20260511-091000-000000001");
        assert_eq!(
            shell.running_session_id(),
            Some("20260511-090000-000000001")
        );
        assert_eq!(shell.open_workspace_count(), 2);
        assert_eq!(
            shell
                .workspace("20260511-091000-000000001")
                .expect("workspace")
                .current_run_id(),
            None
        );
    });
}

#[test]
#[serial]
fn focus_changes_preserve_workspace_ui_state() {
    with_temp_root(|| {
        let first = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(first);

        shell
            .workspace_mut("20260511-090000-000000001")
            .expect("workspace")
            .set_ui_probe_state("draft input", 4, Some(11));
        shell
            .open_session("20260511-091000-000000001")
            .expect("open");
        shell
            .workspace_mut("20260511-091000-000000001")
            .expect("workspace")
            .set_ui_probe_state("other input", 1, Some(22));

        shell
            .focus_session("20260511-090000-000000001")
            .expect("focus");

        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .ui_probe_state(),
            ("draft input".to_string(), 4, Some(11))
        );
        assert_eq!(shell.focused_session_id(), "20260511-090000-000000001");
    });
}

#[test]
#[serial]
fn open_workspace_updates_from_events_not_disk_polling() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(initial);

        let disk_state = session("20260511-090000-000000001", Phase::Done);
        disk_state.save().expect("save changed state to disk");
        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .phase(),
            Phase::WaitingToImplement
        );

        shell.apply_event(ShellEvent::SessionStateChanged {
            session_id: "20260511-090000-000000001".into(),
            state: Box::new(disk_state),
        });

        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .phase(),
            Phase::Done
        );
    });
}

#[test]
#[serial]
fn sidebar_lists_non_archived_non_cancelled_sessions_in_creation_order() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        save_session("20260511-093000-000000001", Phase::Done);
        save_session("20260511-092000-000000001", Phase::BlockedNeedsUser);
        let cancelled = session("20260511-094000-000000001", Phase::Cancelled);
        cancelled.save().expect("save cancelled");
        let mut archived = session("20260511-095000-000000001", Phase::WaitingToImplement);
        archived.archived = true;
        archived.save().expect("save archived");
        let mut shell = shell_for(initial);

        shell.toggle_sessions_sidebar().expect("toggle");
        let view = shell.sidebar_view();
        let rows: Vec<_> = view
            .rows
            .iter()
            .map(|row| row.session_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            rows,
            vec![
                "20260511-090000-000000001",
                "20260511-092000-000000001",
                "20260511-093000-000000001",
            ]
        );
    });
}

#[test]
#[serial]
fn sessions_palette_command_toggles_only_sidebar_and_returns_focus_to_workspace() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(initial);

        assert_eq!(
            shell
                .execute_shell_palette_command("sessions")
                .expect("open"),
            ShellCommandOutcome::Consumed
        );
        shell.focus_sidebar();
        assert!(shell.sidebar_view().visible);
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Sidebar);
        assert_eq!(shell.focused_session_id(), "20260511-090000-000000001");
        assert_eq!(shell.open_workspace_count(), 1);

        assert_eq!(
            shell
                .execute_shell_palette_command("sessions")
                .expect("hide"),
            ShellCommandOutcome::Consumed
        );
        assert!(!shell.sidebar_view().visible);
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Workspace);
        assert_eq!(shell.focused_session_id(), "20260511-090000-000000001");
        assert_eq!(shell.open_workspace_count(), 1);
    });
}

#[test]
#[serial]
fn sidebar_focus_switching_is_active_only_while_sidebar_visible() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(initial);

        assert_eq!(
            shell
                .handle_shell_command(key(UiKeyCode::Right), false)
                .expect("right"),
            ShellCommandOutcome::Unhandled
        );
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Workspace);

        shell.toggle_sessions_sidebar().expect("toggle");
        assert_eq!(
            shell
                .handle_shell_command(key(UiKeyCode::Right), false)
                .expect("right"),
            ShellCommandOutcome::Consumed
        );
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Sidebar);

        assert_eq!(
            shell
                .handle_shell_command(key(UiKeyCode::Left), false)
                .expect("left"),
            ShellCommandOutcome::Consumed
        );
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Workspace);
    });
}

#[test]
#[serial]
fn sidebar_keyboard_navigation_selects_and_opens_without_eager_loading() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        save_session("20260511-091000-000000001", Phase::BlockedNeedsUser);
        save_session("20260511-092000-000000001", Phase::Done);
        let mut shell = shell_for(initial);

        shell.toggle_sessions_sidebar().expect("toggle");
        shell.focus_sidebar();
        assert_eq!(shell.sidebar_view().rows.len(), 3);
        assert_eq!(shell.open_workspace_count(), 1);

        shell
            .handle_shell_command(key(UiKeyCode::Down), false)
            .expect("down");
        assert_eq!(shell.sidebar_view().selected_index, 1);
        assert_eq!(shell.open_workspace_count(), 1);

        shell
            .handle_shell_command(key(UiKeyCode::Up), false)
            .expect("up");
        assert_eq!(shell.sidebar_view().selected_index, 0);

        shell
            .handle_shell_command(key(UiKeyCode::Down), false)
            .expect("down");
        shell
            .handle_shell_command(key(UiKeyCode::Enter), false)
            .expect("enter");

        assert_eq!(shell.focused_session_id(), "20260511-091000-000000001");
        assert_eq!(shell.open_workspace_count(), 2);
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Workspace);
    });
}

#[test]
#[serial]
fn esc_from_sidebar_focus_hides_sidebar_after_modal_gets_first_chance() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(initial);

        shell.toggle_sessions_sidebar().expect("toggle");
        shell.focus_sidebar();

        // Esc with modal_open=true should NOT hide sidebar (modal gets precedence)
        shell
            .handle_shell_command(key(UiKeyCode::Esc), true)
            .expect("esc");
        assert!(shell.sidebar_view().visible);
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Sidebar);

        // Esc with modal_open=false SHOULD hide sidebar
        shell
            .handle_shell_command(key(UiKeyCode::Esc), false)
            .expect("esc");
        assert!(!shell.sidebar_view().visible);
        assert_eq!(shell.sidebar_view().focus, ShellFocus::Workspace);
    });
}
