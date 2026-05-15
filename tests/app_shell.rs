use codexize::app::AppStartupOrigin;
use codexize::app_runtime::{AppCommand, UiKey, UiKeyCode};
use codexize::app_shell::{
    AppShell, ShellCommandOutcome, ShellEvent, ShellFocus, ShellImplementationAction,
};
use codexize::data::config::Config;
use codexize::state::{RunRecord, RunStatus, SessionState, Stage};
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

fn session(id: &str, stage: Stage) -> SessionState {
    let mut state = SessionState::new(id.to_string());
    state.idea_text = Some(format!("idea for {id}"));
    state.current_stage = stage;
    state
}

fn save_session(id: &str, stage: Stage) -> SessionState {
    let state = session(id, stage);
    state.save().expect("save session");
    state
}

fn running_state(id: &str, stage: Stage, run_id: u64) -> SessionState {
    let mut state = session(id, stage);
    state.agent_runs.push(RunRecord {
        id: run_id,
        stage: "sharding".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "test-model".to_string(),
        subscription_label: "test-vendor".to_string(),
        window_name: "[Sharding] test-model".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: codexize::data::adapters::EffortLevel::Normal,
        effort_mapping: codexize::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: codexize::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state
}

fn shell_for(initial: SessionState) -> AppShell {
    let mut config = Config::baked_defaults();
    config.providers.set(Vec::new());
    AppShell::new(initial, AppStartupOrigin::Default, Arc::new(config)).expect("shell")
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
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
        let first = save_session("20260511-090000-000000001", Stage::ShardingRunning);
        save_session("20260511-091000-000000001", Stage::WaitingToImplement);
        let mut shell = shell_for(first);
        shell.apply_event(ShellEvent::SessionStateChanged {
            session_id: "20260511-090000-000000001".into(),
            state: Box::new(running_state(
                "20260511-090000-000000001",
                Stage::ShardingRunning,
                7,
            )),
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
        let first = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
        save_session("20260511-091000-000000001", Stage::WaitingToImplement);
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
        let mut shell = shell_for(initial);

        let disk_state = session("20260511-090000-000000001", Stage::Done);
        disk_state.save().expect("save changed state to disk");
        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .stage(),
            Stage::WaitingToImplement
        );

        shell.apply_event(ShellEvent::SessionStateChanged {
            session_id: "20260511-090000-000000001".into(),
            state: Box::new(disk_state),
        });

        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .stage(),
            Stage::Done
        );
    });
}

#[test]
#[serial]
fn sidebar_lists_non_archived_non_cancelled_sessions_in_creation_order() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
        save_session("20260511-093000-000000001", Stage::Done);
        save_session("20260511-092000-000000001", Stage::BlockedNeedsUser);
        let cancelled = session("20260511-094000-000000001", Stage::Cancelled);
        cancelled.save().expect("save cancelled");
        let mut archived = session("20260511-095000-000000001", Stage::WaitingToImplement);
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
        let mut titled = session("20260512-091000-000000001", Stage::Done);
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
        save_session("20260511-091000-000000001", Stage::BlockedNeedsUser);
        save_session("20260511-092000-000000001", Stage::Done);
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
        let initial = save_session("20260511-090000-000000001", Stage::WaitingToImplement);
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
fn sidebar_shows_every_non_archived_non_cancelled_stage() {
    // AC-8 / spec §"Sidebar list": the sidebar must list non-archived sessions
    // only and show enough state to distinguish focused, open, waiting,
    // running, blocked, done, and cancelled rows. Cancelled sessions are
    // excluded. Verify that every non-archived, non-cancelled stage appears.
    with_temp_root(|| {
        let stages: Vec<Stage> = vec![
            Stage::IdeaInput,
            Stage::BrainstormRunning,
            Stage::SpecReviewRunning,
            Stage::SpecReviewPaused,
            Stage::PlanningRunning,
            Stage::PlanReviewRunning,
            Stage::PlanReviewPaused,
            Stage::WaitingToImplement,
            Stage::RepoStateUpdateRunning,
            Stage::ShardingRunning,
            Stage::ImplementationRound(1),
            Stage::ReviewRound(1),
            Stage::BuilderRecovery(1),
            Stage::BuilderRecoveryPlanReview(1),
            Stage::BuilderRecoverySharding(1),
            Stage::Simplification(1),
            Stage::FinalValidation(1),
            Stage::DreamingPending,
            Stage::Dreaming(1),
            Stage::BlockedNeedsUser,
            Stage::Done,
        ];

        for (i, stage) in stages.iter().enumerate() {
            let id = format!("20260511-{i:02}0000-000000001");
            save_session(&id, *stage);
        }

        let initial = save_session("20260511-990000-000000001", Stage::WaitingToImplement);
        let mut shell = shell_for(initial);
        shell.toggle_sessions_sidebar().expect("toggle");
        let view = shell.sidebar_view();

        let row_stages: Vec<Stage> = view.rows.iter().map(|r| r.stage).collect();
        for stage in &stages {
            assert!(row_stages.contains(stage), "sidebar must include {stage:?}");
        }
        // Cancelled must NOT appear.
        assert!(
            !row_stages.contains(&Stage::Cancelled),
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
        let running = save_session("20260511-090000-000000001", Stage::ShardingRunning);
        save_session("20260511-091000-000000001", Stage::WaitingToImplement);
        let mut shell = shell_for(running);
        shell.apply_event(ShellEvent::SessionStateChanged {
            session_id: "20260511-090000-000000001".into(),
            state: Box::new(running_state(
                "20260511-090000-000000001",
                Stage::ShardingRunning,
                42,
            )),
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
        let running = save_session("20260511-090000-000000001", Stage::ShardingRunning);
        save_session("20260511-091000-000000001", Stage::PlanningRunning);
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
                stage: Stage::ShardingRunning,
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
                .stage(),
            Stage::PlanningRunning
        );
    });
}

#[test]
#[serial]
fn scheduler_tick_blocks_later_implementation_behind_earlier_blocked_session() {
    with_temp_root(|| {
        let blocked = save_session("20260511-090000-000000001", Stage::BlockedNeedsUser);
        save_session("20260511-091000-000000001", Stage::WaitingToImplement);
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
                .current_stage,
            Stage::WaitingToImplement,
            "later waiting session must not dispatch while the earlier head is blocked"
        );
    });
}

#[test]
#[serial]
fn scheduler_tick_skips_cancelled_and_dispatches_oldest_waiting_session() {
    with_temp_root(|| {
        let initial = save_session("20260511-090000-000000001", Stage::Cancelled);
        save_session("20260511-091000-000000001", Stage::WaitingToImplement);
        save_session("20260511-092000-000000001", Stage::WaitingToImplement);
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
                stage: Stage::ShardingRunning,
            }
        );
        assert_eq!(
            SessionState::load("20260511-091000-000000001")
                .expect("load dispatched")
                .current_stage,
            Stage::ShardingRunning
        );
        assert_eq!(
            SessionState::load("20260511-092000-000000001")
                .expect("load later")
                .current_stage,
            Stage::WaitingToImplement,
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
        let mut state = session("20260511-090000-000000001", Stage::PlanningRunning);
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
            status: RunStatus::Running,
            error: None,
            effort: codexize::data::adapters::EffortLevel::Normal,
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
        // Step 6: persisted Running runs are backfilled to Failed on resume
        // and the in-memory FSM/current_run_id always starts None. The
        // no-duplicate-run invariant above is the load-bearing assertion;
        // the workspace's `current_run_id` is intentionally None until a
        // launch fires in-process.
        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("ws")
                .current_run_id(),
            None,
            "workspace current_run_id stays None across ticks without a launch"
        );
    });
}
