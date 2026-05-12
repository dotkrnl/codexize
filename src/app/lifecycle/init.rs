use super::super::{
    App, AppStartupOrigin,
    models::spawn_refresh,
    models_area, palette, startup_cache_has_expired_section,
    state::ModelRefreshState,
    status_line,
    tree::{build_tree, current_node_index, node_key_at_path},
};
use crate::{
    cache,
    state::{self as session_state, Phase, SessionState},
    tasks,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
impl App {
    pub fn new(state: SessionState) -> Self {
        Self::new_with_startup_origin(state, AppStartupOrigin::Default)
    }
    pub fn new_with_startup_origin(state: SessionState, startup_origin: AppStartupOrigin) -> Self {
        let config = Arc::new(crate::data::config::load_or_default().unwrap_or_else(|e| {
            eprintln!("config: using defaults: {e}");
            crate::data::config::Config::baked_defaults()
        }));
        Self::new_with_startup_origin_and_config(state, startup_origin, config)
    }
    pub fn new_with_startup_origin_and_config(
        mut state: SessionState,
        startup_origin: AppStartupOrigin,
        config: Arc<crate::data::config::Config>,
    ) -> Self {
        let ntfy_params =
            crate::data::notifications::NotificationParams::from_view(&config.ntfy_view());
        let acp_config = crate::acp::AcpConfig::from_config_views(
            &config.acp.agents,
            &config.acp_install_view(),
        );
        let messages = SessionState::load_messages(&state.session_id).unwrap_or_default();
        let paths_view = config.paths_view();
        let memory_view = config.memory_view();
        let ui_view = config.ui_view();
        if state.builder.task_titles.is_empty() {
            // Same fallback as `App::session_dir`: only honor the
            // configured `sessions_root` when explicit, otherwise read
            // from the project-local `.codexize/sessions` the runner uses.
            let tasks_path = if config.paths.sessions_root.is_explicit() {
                paths_view.sessions_root.join(&state.session_id)
            } else {
                session_state::codexize_root()
                    .join("sessions")
                    .join(&state.session_id)
            };
            let tasks_path = tasks_path.join("artifacts").join("tasks.toml");
            if let Ok(parsed) = tasks::validate(&tasks_path) {
                session_state::load_task_titles_if_empty(
                    &mut state,
                    parsed.tasks.into_iter().map(|t| (t.id, t.title)),
                );
            }
        }
        let nodes = build_tree(&state);
        let providers = config.providers.value().clone();
        let current = current_node_index(&nodes);
        let selected_key = node_key_at_path(&nodes, &[current]);
        let failed_models = Self::rebuild_failed_models(&state);
        let project_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        let mut app = Self {
            state,
            nodes,
            visible_rows: Vec::new(),
            models: Vec::new(),
            model_refresh: ModelRefreshState::Fetching {
                rx: spawn_refresh(
                    paths_view.cache_root.clone(),
                    acp_config.available_clis(),
                    providers.clone(),
                ),
                started_at: Instant::now(),
            },
            selected: current,
            selected_key,
            collapsed_overrides: BTreeMap::new(),
            viewport_top: 0,
            follow_tail: true,
            explicit_viewport_scroll: false,
            progress_follow_active: true,
            tail_detach_baseline: None,
            body_inner_height: 0,
            body_inner_width: 0,
            split_target: None,
            split_follow_tail: true,
            split_scroll_offset: 0,
            split_fullscreen: false,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            pending_view_path: None,
            confirm_back: false,
            startup_origin,
            run_launched: false,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            agent_content_hash: 0,
            agent_last_change: None,
            spinner_tick: 0,
            live_summary_spinner_visible: false,
            live_summary_path: None,
            live_summary_watcher: None,
            live_summary_change_events: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            cache_watcher: None,
            pending_drain_deadline: None,
            pending_termination: None,
            pending_quit_confirmation_run_id: None,
            pending_cancel_confirmation: false,
            interactive_exit_prompt_dismissed_at: None,
            pending_app_exit: false,
            pending_shell_command: None,
            current_run_id: None,
            failed_models,
            runner_supervisor: app_runner_supervisor(&config),
            runner_config: crate::runner::RunnerConfig {
                full_review_interval: config.runner_view().full_review_interval,
            },
            pending_yolo_toggle_gate: None,
            yolo_exit_issued: HashSet::new(),
            yolo_exit_observations: HashMap::new(),
            watchdog: super::watchdog::WatchdogRegistry::from_env(),
            notification_runtime: crate::data::notifications::NotificationRuntime::new(ntfy_params),
            interactive_wait_marker: None,
            config,
            paths: paths_view,
            memory_view,
            ui_view,
            #[cfg(test)]
            test_launch_harness: None,
            messages,
            status_line: Rc::new(RefCell::new(status_line::StatusLine::new())),
            prev_models_mode: models_area::ModelsAreaMode::default(),
            palette: palette::PaletteState::default(),
            command_return_target: None,
            config_panel: None,
            last_config_section: None,
            project_name,
        };
        app.rebuild_visible_rows();
        app.restore_selection(app.selected_key.clone(), app.selected);
        // Once-per-launch journal retention sweep: drop monthly entries older
        // than `[memory] journal_retention_months`. Failures are logged-only
        // — pruning is best-effort and must not block session startup.
        let memory_root = app.memory_root();
        let retention = app.memory_view.journal_retention_months;
        match crate::data::memory::prune_journal_entries(&memory_root, retention) {
            Ok(0) => {}
            Ok(n) => {
                let _ = app.state.log_event(format!(
                    "journal_pruned: removed={n} retention_months={retention}"
                ));
            }
            Err(err) => {
                let _ = app.state.log_event(format!("journal_prune_failed: {err}"));
            }
        }
        // Populate the model strip immediately from whatever the cache holds.
        // The background refresh spawned above will replace this if any section
        // is expired.
        let loaded = cache::load(&app.paths.cache_root);
        let cached = crate::data::selection_assembly::assemble_from_loaded(
            &loaded,
            &acp_config.available_clis(),
            &providers,
        );
        if !cached.is_empty() {
            let cache_has_expired_section = startup_cache_has_expired_section(&loaded);
            app.set_models(cached);
            if !cache_has_expired_section {
                app.model_refresh = ModelRefreshState::Idle(Instant::now());
            }
        }
        // Install the cache watcher so atomic publishes from other instances
        // refresh the model strip without restart. The watcher seeds itself
        // with the mtime we just loaded; subsequent advances trigger a
        // single debounced reload.
        app.setup_cache_watcher();
        if let Ok(run_id) = session_state::resume_running_runs(&mut app.state) {
            app.current_run_id = run_id;
            app.run_launched = run_id.is_some();
            if let Some(rid) = run_id {
                if let Some(run) = app.state.agent_runs.iter().find(|r| r.id == rid).cloned() {
                    app.live_summary_path = Some(app.live_summary_path_for(&run));
                    app.prime_yolo_exit_tracking(&run);
                }
                app.read_live_summary_pipeline();
            }
            app.messages = SessionState::load_messages(&app.state.session_id).unwrap_or_default();
            app.rebuild_tree_view(None);
            app.maybe_refocus_to_progress();
        }
        // Resume validation: if the session was interrupted mid-guard-decision,
        // restore the modal or fail closed.
        if app.state.current_phase == Phase::GitGuardPending {
            if app.state.pending_guard_decision.is_none() {
                app.record_agent_error("guard pending state missing on resume".to_string());
                app.clear_builder_recovery_context();
                let _ = app.transition_to_blocked(crate::state::BlockOrigin::GitGuard);
                let _ = app.state.save();
            }
        } else if app.state.pending_guard_decision.is_some() {
            // Stale: pending decision with no matching phase — clear it.
            let _ = app.state.log_event(
                "warning: clearing stale pending_guard_decision (phase mismatch on resume)"
                    .to_string(),
            );
            session_state::clear_pending_guard_decision(&mut app.state);
            let _ = app.state.save();
        }
        // Orphan sweep: remove stale live_summary.*.txt files that do not
        // correspond to a Running run record.
        {
            let artifacts_dir = app.session_dir().join("artifacts");
            let running_keys: std::collections::HashSet<String> = app
                .state
                .agent_runs
                .iter()
                .filter(|run| run.status == crate::state::RunStatus::Running)
                .map(|run| App::run_key_for(&run.stage, run.task_id, run.round, run.attempt))
                .collect();
            if let Ok(entries) = std::fs::read_dir(&artifacts_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str == "live_summary.txt" {
                        let _ = std::fs::remove_file(entry.path());
                        continue;
                    }
                    if name_str.starts_with("live_summary.")
                        && name_str.ends_with(".txt")
                        && let Some(run_key) = name_str
                            .strip_prefix("live_summary.")
                            .and_then(|s| s.strip_suffix(".txt"))
                        && !running_keys.contains(run_key)
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        // Stamp archival: move old finish stamps to archive/ at session start.
        // Stamps older than the oldest Running record are archived (best effort).
        {
            let finish_dir = app.session_dir().join("artifacts").join("run-finish");
            let archive_dir = finish_dir.join("archive");
            let oldest_running_timestamp = app
                .state
                .agent_runs
                .iter()
                .filter(|run| run.status == crate::state::RunStatus::Running)
                .map(|run| run.started_at)
                .min();
            if let Some(cutoff) = oldest_running_timestamp
                && let Ok(entries) = std::fs::read_dir(&finish_dir)
            {
                for entry in entries.flatten() {
                    if !entry.path().is_file() {
                        continue;
                    }
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if !name_str.ends_with(".toml") {
                        continue;
                    }
                    if let Ok(stamp) = crate::runner::read_finish_stamp(&entry.path())
                        && let Ok(finished) =
                            chrono::DateTime::parse_from_rfc3339(&stamp.finished_at)
                    {
                        let finished_utc = finished.with_timezone(&chrono::Utc);
                        if finished_utc < cutoff {
                            let _ = std::fs::create_dir_all(&archive_dir);
                            let dest = archive_dir.join(&name);
                            let _ = std::fs::rename(entry.path(), dest);
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        for run in app
            .state
            .agent_runs
            .iter()
            .filter(|run| run.status == crate::state::RunStatus::Running)
        {
            crate::runner::register_test_run_id(&run.window_name, run.id);
        }
        let _ = app.setup_watcher();
        app
    }
}
#[cfg(test)]
fn app_runner_supervisor(
    config: &std::sync::Arc<crate::data::config::Config>,
) -> crate::runner::Supervisor {
    let _ = config;
    crate::runner::Supervisor::shared_for_test()
}
#[cfg(not(test))]
fn app_runner_supervisor(
    config: &std::sync::Arc<crate::data::config::Config>,
) -> crate::runner::Supervisor {
    crate::runner::Supervisor::new(config.clone())
}
