//! Minimal test helpers for the App-internal test suites.
//!
//! Replaces the bespoke `test_harness.rs` fixture machinery. Only carries
//! the small primitives the surviving snapshot tests and split-sync test
//! still need: a tempdir wrapper that pins `CODEXIZE_ROOT` (with optional
//! cwd swap for prompts that probe the working directory), an `App`
//! constructor that wires up the visible-row cache, and a key-event helper.

use super::tree::{build_tree, current_node_index, node_key_at_path};
use super::*;

/// Pin `CODEXIZE_ROOT` to a tempdir for the duration of `f`. Use this for
/// any test that calls into App methods which save session state, write
/// events, or otherwise touch `session_dir(...)`. Without this, those
/// writes leak into the host repo's `.codexize/sessions/` directory.
/// Serialized via `test_fs_lock` since env mutation is process-global.
pub(crate) fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::TempDir::new().expect("tempdir");
    let prev_root = std::env::var_os("CODEXIZE_ROOT");

    // SAFETY: env mutation is serialized by `test_fs_lock`.
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prev_root {
            Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    result.expect("test panicked")
}

/// Pin `CODEXIZE_ROOT` to a tempdir and chdir into it for the duration of
/// `f`. Required by prompts that read `CLAUDE.md`/`AGENTS.md` from the
/// current working directory — the cwd must be empty so the rendered prompt
/// snapshot is independent of what's checked into the host repo. Serialized
/// via `test_fs_lock` since chdir is process-global.
pub(crate) fn with_temp_root_and_cwd<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::TempDir::new().expect("tempdir");
    let prev_root = std::env::var_os("CODEXIZE_ROOT");
    let prev_cwd = std::env::current_dir().ok();

    // SAFETY: env + cwd mutation is serialized by `test_fs_lock`.
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let chdir_result = std::env::set_current_dir(temp.path());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(temp.path())));
    if let (Ok(()), Some(p)) = (&chdir_result, prev_cwd) {
        let _ = std::env::set_current_dir(p);
    }
    unsafe {
        match prev_root {
            Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    chdir_result.expect("chdir into tempdir for snapshot test");
    result.expect("test panicked")
}

pub(crate) fn mk_app(state: crate::state::SessionState) -> App {
    let nodes = build_tree(&state);
    let current = current_node_index(&nodes);
    let selected_key = node_key_at_path(&nodes, &[current]);
    let mut app = App {
        state,
        nodes,
        visible_rows: Vec::new(),
        models: Vec::new(),
        model_refresh: ModelRefreshState::Idle(Instant::now()),
        selected: 0,
        selected_key,
        collapsed_overrides: BTreeMap::new(),
        viewport_top: 0,
        follow_tail: true,
        explicit_viewport_scroll: false,
        progress_follow_active: true,
        tail_detach_baseline: None,
        body_inner_height: 30,
        body_inner_width: 80,
        split_target: None,
        split_follow_tail: true,
        split_scroll_offset: 0,
        split_fullscreen: false,
        input_mode: false,
        input_buffer: String::new(),
        input_cursor: 0,
        pending_view_path: None,
        confirm_back: false,
        startup_origin: AppStartupOrigin::Default,
        run_launched: true,
        quota_errors: Vec::new(),
        quota_retry_delay: Duration::from_secs(60),
        agent_line_count: 0,
        agent_content_hash: 0,
        agent_last_change: None,
        spinner_tick: 0,
        live_summary_spinner_visible: false,
        live_summary_watcher: None,
        live_summary_change_events: None,
        live_summary_path: None,
        live_summary_cached_text: String::new(),
        live_summary_cached_mtime: None,
        pending_drain_deadline: None,
        pending_termination: None,
        pending_quit_confirmation_run_id: None,
        interactive_exit_prompt_dismissed_at: None,
        pending_app_exit: false,
        current_run_id: Some(2),
        failed_models: HashMap::new(),
        pending_yolo_toggle_gate: None,
        yolo_exit_issued: HashSet::new(),
        yolo_exit_observations: HashMap::new(),
        runner_supervisor: crate::runner::Supervisor::shared_for_test(),
        runner_config: crate::runner::RunnerConfig::default(),
        notification_runtime: crate::data::notifications::NotificationRuntime::new_disabled(),
        interactive_wait_marker: None,
        config: std::sync::Arc::new(crate::data::config::Config::baked_defaults()),
        paths: crate::data::config::Config::baked_defaults().paths_view(),
        memory_view: crate::data::config::Config::baked_defaults().memory_view(),
        ui_view: crate::data::config::Config::baked_defaults().ui_view(),
        watchdog: super::watchdog::WatchdogRegistry::new(),
        test_launch_harness: None,
        messages: Vec::new(),
        status_line: Rc::new(RefCell::new(status_line::StatusLine::new())),
        prev_models_mode: models_area::ModelsAreaMode::default(),
        palette: palette::PaletteState::default(),
        command_return_target: None,
        config_panel: None,
    };
    for run in app
        .state
        .agent_runs
        .iter()
        .filter(|run| run.status == crate::state::RunStatus::Running)
    {
        crate::runner::register_test_run_id(&run.window_name, run.id);
    }
    app.rebuild_visible_rows();
    app.restore_selection(app.selected_key.clone(), app.selected);
    app
}

pub(crate) fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}
