use codexize::app::AppStartupOrigin;
use codexize::app_runtime::{AppCommand, UiKey, UiKeyCode};
use codexize::app_shell::{
    AppShell, ShellCommandOutcome, ShellEvent, ShellFocus, ShellImplementationAction,
};
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
fn sidebar_rows_expose_mm_dd_title_labels() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let mut titled = session("20260512-091000-000000001", Phase::Done);
        titled.title = Some("Ship queued sharding".to_string());
        titled.save().expect("save titled");
        let mut shell = shell_for(initial);

        shell.toggle_sessions_sidebar().expect("toggle");
        let rows = shell.sidebar_view().rows;
        let titled_row = rows
            .iter()
            .find(|row| row.session_id == "20260512-091000-000000001")
            .expect("titled row");

        assert_eq!(titled_row.date_label, "05/12");
        assert_eq!(titled_row.title, "Ship queued sharding");
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

#[test]
#[serial]
fn sidebar_shows_every_non_archived_non_cancelled_phase() {
    // AC-8 / spec §"Sidebar list": the sidebar must list non-archived sessions
    // only and show enough state to distinguish focused, open, waiting,
    // running, blocked, done, and cancelled rows. Cancelled sessions are
    // excluded. Verify that every non-archived, non-cancelled phase appears.
    with_temp_root(|| {
        let phases: Vec<Phase> = vec![
            Phase::IdeaInput,
            Phase::BrainstormRunning,
            Phase::SpecReviewRunning,
            Phase::SpecReviewPaused,
            Phase::PlanningRunning,
            Phase::PlanReviewRunning,
            Phase::PlanReviewPaused,
            Phase::WaitingToImplement,
            Phase::RepoStateUpdateRunning,
            Phase::ShardingRunning,
            Phase::ImplementationRound(1),
            Phase::ReviewRound(1),
            Phase::BuilderRecovery(1),
            Phase::BuilderRecoveryPlanReview(1),
            Phase::BuilderRecoverySharding(1),
            Phase::Simplification(1),
            Phase::FinalValidation(1),
            Phase::DreamingPending,
            Phase::Dreaming(1),
            Phase::BlockedNeedsUser,
            Phase::Done,
        ];

        for (i, phase) in phases.iter().enumerate() {
            let id = format!("20260511-{:02}0000-000000001", i);
            save_session(&id, *phase);
        }

        let initial = session("20260511-000000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(initial);
        shell.toggle_sessions_sidebar().expect("toggle");
        let view = shell.sidebar_view();

        let row_phases: Vec<Phase> = view.rows.iter().map(|r| r.phase).collect();
        for phase in &phases {
            assert!(row_phases.contains(phase), "sidebar must include {phase:?}");
        }
        // Cancelled must NOT appear.
        assert!(
            !row_phases.contains(&Phase::Cancelled),
            "sidebar must exclude Cancelled"
        );
    });
}

#[test]
#[serial]
fn background_run_continues_when_focus_switches_to_another_session() {
    // AC-8 / spec §"Focused vs running": a background implementation run
    // continues when the operator focuses another session. The running
    // session's shell-level tracking must be preserved while focus changes.
    with_temp_root(|| {
        let running = save_session("20260511-090000-000000001", Phase::ShardingRunning);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(running);

        // Simulate a background run starting on the first session.
        shell.apply_event(ShellEvent::RunStarted {
            session_id: "20260511-090000-000000001".into(),
            run_id: 42,
        });
        assert_eq!(
            shell.running_session_id(),
            Some("20260511-090000-000000001")
        );

        // Focus the second session — the background run must continue.
        shell
            .focus_session("20260511-091000-000000001")
            .expect("focus");
        assert_eq!(shell.focused_session_id(), "20260511-091000-000000001");
        assert_eq!(
            shell.running_session_id(),
            Some("20260511-090000-000000001"),
            "running session must not change on focus switch"
        );

        // The sidebar must still mark the running session correctly.
        shell.toggle_sessions_sidebar().expect("toggle");
        let rows = shell.sidebar_view().rows;
        let running_row = rows
            .iter()
            .find(|r| r.session_id == "20260511-090000-000000001")
            .expect("running session in sidebar");
        assert!(
            running_row.running,
            "running session must have running indicator"
        );
        let focused_row = rows
            .iter()
            .find(|r| r.session_id == "20260511-091000-000000001")
            .expect("focused session in sidebar");
        assert!(
            focused_row.focused,
            "focused session must have focused indicator"
        );
        assert!(!focused_row.running, "focused session must not be running");
    });
}

#[test]
#[serial]
fn scheduler_tick_continues_planning_while_implementation_lane_is_occupied() {
    with_temp_root(|| {
        let running = save_session("20260511-090000-000000001", Phase::ShardingRunning);
        save_session("20260511-091000-000000001", Phase::PlanningRunning);
        let mut shell = shell_for(running);
        shell
            .open_session("20260511-091000-000000001")
            .expect("open planning");
        shell
            .focus_session("20260511-090000-000000001")
            .expect("refocus running");

        let report = shell.run_scheduler_tick().expect("scheduler tick");

        assert_eq!(
            report.planning_session_ids,
            vec!["20260511-091000-000000001".to_string()]
        );
        assert_eq!(
            report.implementation,
            ShellImplementationAction::LaneOccupied {
                session_id: "20260511-090000-000000001".to_string(),
                phase: Phase::ShardingRunning,
            }
        );
        assert!(
            shell.workspace("20260511-091000-000000001").is_some(),
            "scheduler should load the later planning workspace to continue its automation"
        );
        assert_eq!(
            shell
                .workspace("20260511-091000-000000001")
                .expect("planning workspace")
                .phase(),
            Phase::PlanningRunning
        );
    });
}

#[test]
#[serial]
fn scheduler_tick_blocks_later_implementation_behind_earlier_blocked_session() {
    with_temp_root(|| {
        let blocked = save_session("20260511-090000-000000001", Phase::BlockedNeedsUser);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(blocked);
        shell
            .open_session("20260511-091000-000000001")
            .expect("open later waiting");
        shell
            .focus_session("20260511-090000-000000001")
            .expect("refocus blocked");

        let report = shell.run_scheduler_tick().expect("scheduler tick");

        assert_eq!(
            report.implementation,
            ShellImplementationAction::BlockedByHead {
                session_id: "20260511-090000-000000001".to_string(),
            }
        );
        assert_eq!(
            SessionState::load("20260511-091000-000000001")
                .expect("load later")
                .current_phase,
            Phase::WaitingToImplement,
            "later waiting session must not dispatch while the earlier head is blocked"
        );
    });
}

#[test]
#[serial]
fn scheduler_tick_skips_cancelled_and_dispatches_oldest_waiting_session() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Phase::Cancelled);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        save_session("20260511-092000-000000001", Phase::WaitingToImplement);
        let mut shell = shell_for(initial);
        shell
            .open_session("20260511-091000-000000001")
            .expect("open first waiting");
        shell
            .open_session("20260511-092000-000000001")
            .expect("open second waiting");
        shell
            .focus_session("20260511-090000-000000001")
            .expect("refocus cancelled");

        let report = shell.run_scheduler_tick().expect("scheduler tick");

        assert_eq!(
            report.implementation,
            ShellImplementationAction::DispatchedWaiting {
                session_id: "20260511-091000-000000001".to_string(),
                phase: Phase::ShardingRunning,
            }
        );
        assert_eq!(
            SessionState::load("20260511-091000-000000001")
                .expect("load dispatched")
                .current_phase,
            Phase::ShardingRunning
        );
        assert_eq!(
            SessionState::load("20260511-092000-000000001")
                .expect("load later")
                .current_phase,
            Phase::WaitingToImplement,
            "oldest eligible waiting session should dispatch first"
        );
    });
}

#[test]
#[serial]
fn scheduler_tick_does_not_duplicate_background_planning_run_when_models_loaded() {
    use chrono::Utc;
    use codexize::state::RunRecord;
    with_temp_root(|| {
        let mut state = session("20260511-090000-000000001", Phase::PlanningRunning);
        state.idea_text = Some("idea".to_string());
        let run = RunRecord {
            id: 42,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            subscription_label: "test".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            status: codexize::state::RunStatus::Running,
            error: None,
            effort: codexize::adapters::EffortLevel::Normal,
            effort_mapping: codexize::data::config::schema::EffortMapping::default(),
            effort_eligible: true,
            modes: codexize::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        };
        state.agent_runs.push(run);
        state.save().expect("save");

        let mut shell = shell_for(state);

        // Scheduler tick 1: background planning session already has a
        // Running agent_run. The shell rebuilds a fresh App for each
        // background tick; `drive_scheduler_session` must restore the
        // workspace's preserved current_run_id so `maybe_auto_launch`
        // recognises the active run and skips re-dispatch.
        let report1 = shell.run_scheduler_tick().expect("tick 1");
        assert!(
            report1
                .planning_session_ids
                .contains(&"20260511-090000-000000001".to_string()),
            "planning session should be scanned"
        );
        let loaded1 = SessionState::load("20260511-090000-000000001").expect("load 1");
        assert_eq!(
            loaded1.agent_runs.len(),
            1,
            "tick 1: must not duplicate run"
        );

        // Tick 2 confirms the invariant survives repeated ticks.
        let _ = shell.run_scheduler_tick().expect("tick 2");
        let loaded2 = SessionState::load("20260511-090000-000000001").expect("load 2");
        assert_eq!(
            loaded2.agent_runs.len(),
            1,
            "tick 2: must not duplicate run"
        );
        // The workspace's preserved current_run_id must continue to point at
        // the existing run, not be reset to None by the rebuild.
        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("ws")
                .current_run_id(),
            Some(42),
            "workspace must preserve current_run_id across ticks"
        );
    });
}
