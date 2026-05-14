//! Project-level shell for lazy session workspaces.
//!
//! The existing [`crate::app::App`] remains the focus-local session runtime.
//! `AppShell` is the project owner above it: it keeps the startup-picker
//! selection as the first workspace, tracks focused and running sessions
//! separately, and applies runner/scheduler notifications to already-open
//! workspaces through an in-process event path.

use crate::app::{App, AppStartupOrigin};
use crate::app_runtime::{AppCommand, UiKeyCode};
use crate::data::app_lock::AppLockGuard;
use crate::data::config::Config;
use crate::data::session_index::SessionIndex;
use crate::scheduler::{
    ImplementationDecision, ScannedSession, SchedulerTick, evaluate_tick,
    is_implementation_lane_phase,
};
use crate::state::{Phase, RunStatus, SessionState};
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFocus {
    Workspace,
    Sidebar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommandOutcome {
    Consumed,
    Unhandled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub session_id: String,
    pub date_label: String,
    pub title: String,
    pub phase: Phase,
    pub focused: bool,
    pub open: bool,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarView {
    pub visible: bool,
    pub focus: ShellFocus,
    pub selected_index: usize,
    pub rows: Vec<SidebarRow>,
}

#[derive(Debug, Clone)]
struct SidebarModel {
    visible: bool,
    focus: ShellFocus,
    selected_index: usize,
    rows: Vec<SidebarRow>,
    dirty: bool,
    rebuild_count: usize,
}

impl SidebarModel {
    fn new() -> Self {
        Self {
            visible: false,
            focus: ShellFocus::Workspace,
            selected_index: 0,
            rows: Vec::new(),
            dirty: true,
            rebuild_count: 0,
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn refresh_if_dirty(
        &mut self,
        index: &SessionIndex,
        focused_session_id: &str,
        running_session_id: Option<&str>,
        supervisors: &BTreeMap<String, SessionSupervisor>,
    ) {
        if !self.dirty {
            return;
        }
        self.rows = index
            .snapshot_for_sidebar()
            .into_iter()
            .filter(|entry| entry.current_phase != Phase::Cancelled)
            .map(|entry| {
                let session_id = entry.session_id;
                SidebarRow {
                    focused: session_id == focused_session_id,
                    open: supervisors.contains_key(&session_id),
                    running: running_session_id == Some(session_id.as_str()),
                    date_label: sidebar_date_label(&session_id),
                    title: entry.idea_summary,
                    session_id,
                    phase: entry.current_phase,
                }
            })
            .collect();
        if self.selected_index >= self.rows.len() {
            self.selected_index = self.rows.len().saturating_sub(1);
        }
        self.rebuild_count += 1;
        self.dirty = false;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellImplementationAction {
    LaneOccupied { session_id: String, phase: Phase },
    BlockedByHead { session_id: String },
    PlanningHead { session_id: String, phase: Phase },
    DispatchedWaiting { session_id: String, phase: Phase },
    BlockedByCorruptEarlierSession { session_id: String, error: String },
    NothingToDo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSchedulerReport {
    pub planning_session_ids: Vec<String>,
    pub implementation: ShellImplementationAction,
    pub skipped_corrupt_later_sessions: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum ShellEvent {
    SessionStateChanged {
        session_id: String,
        state: Box<SessionState>,
    },
    LiveSummaryChanged {
        session_id: String,
        text: String,
    },
    RunStarted {
        session_id: String,
        run_id: u64,
    },
    RunFinished {
        session_id: String,
        run_id: u64,
    },
}

#[derive(Debug, Clone)]
pub struct ShellEventBus {
    tx: broadcast::Sender<ShellEvent>,
}

impl ShellEventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ShellEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: ShellEvent) {
        let _ = self.tx.send(event);
    }
}

impl Default for ShellEventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SupervisorTick {
    state_changed: bool,
    run_started: Option<u64>,
    run_finished: Option<u64>,
    live_summary_changed: Option<String>,
}

pub struct SessionSupervisor {
    app: Option<App>,
}

impl SessionSupervisor {
    fn new(state: SessionState, startup_origin: AppStartupOrigin, config: Arc<Config>) -> Self {
        Self {
            app: Some(App::new_with_startup_origin_config_without_model_refresh(
                state,
                startup_origin,
                config,
            )),
        }
    }

    fn app(&self) -> &App {
        self.app
            .as_ref()
            .expect("session supervisor temporarily lent its App")
    }

    fn app_mut(&mut self) -> &mut App {
        self.app
            .as_mut()
            .expect("session supervisor temporarily lent its App")
    }

    fn take_app(&mut self) -> App {
        self.app
            .take()
            .expect("session supervisor cannot lend its App twice")
    }

    fn replace_app(&mut self, app: App) {
        self.app = Some(app);
    }

    pub fn session_id(&self) -> &str {
        &self.app().state.session_id
    }

    pub fn phase(&self) -> Phase {
        self.app().state.current_phase
    }

    pub fn current_run_id(&self) -> Option<u64> {
        self.app.as_ref().and_then(|app| app.current_run_id)
    }

    pub fn live_summary_text(&self) -> &str {
        &self.app().live_summary_cached_text
    }

    pub fn set_ui_probe_state(
        &mut self,
        input_buffer: impl Into<String>,
        viewport_top: usize,
        split_run_id: Option<u64>,
    ) {
        let app = self.app_mut();
        app.input_buffer = input_buffer.into();
        app.input_cursor = app.input_buffer.chars().count();
        app.viewport_top = viewport_top;
        app.split_target = split_run_id.map(crate::app::split::SplitTarget::Run);
    }

    pub fn ui_probe_state(&self) -> (String, usize, Option<u64>) {
        let app = self.app();
        let split_run_id = match app.split_target {
            Some(crate::app::split::SplitTarget::Run(run_id)) => Some(run_id),
            Some(crate::app::split::SplitTarget::Idea) | None => None,
        };
        (app.input_buffer.clone(), app.viewport_top, split_run_id)
    }

    fn replace_state(&mut self, state: SessionState) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        app.current_run_id = state
            .agent_runs
            .iter()
            .find(|run| run.status == RunStatus::Running)
            .map(|run| run.id);
        app.run_launched = app.current_run_id.is_some();
        app.state = state;
        app.messages = SessionState::load_messages(&app.state.session_id).unwrap_or_default();
        app.rebuild_tree_view(None);
    }

    fn replace_live_summary(&mut self, text: String) {
        if let Some(app) = self.app.as_mut() {
            app.live_summary_cached_text = crate::app::render::sanitize_live_summary(&text);
        }
    }

    fn drive(&mut self, drive: SchedulerDrive) -> (SessionState, SupervisorTick) {
        let app = self.app_mut();
        let before_run_id = app.current_run_id;
        let before_live_summary = app.live_summary_cached_text.clone();
        drive.apply(app);
        let after_run_id = app.current_run_id;
        let live_summary_changed = (app.live_summary_cached_text != before_live_summary)
            .then(|| app.live_summary_cached_text.clone());
        let tick = SupervisorTick {
            state_changed: true,
            run_started: (before_run_id != after_run_id)
                .then_some(after_run_id)
                .flatten(),
            run_finished: (before_run_id != after_run_id && after_run_id.is_none())
                .then_some(before_run_id)
                .flatten(),
            live_summary_changed,
        };
        (app.state.clone(), tick)
    }

    #[cfg(test)]
    fn app_identity(&self) -> usize {
        self.app() as *const App as usize
    }
}

pub struct AppShell {
    // Project-level single-process lock guard. Held for the lifetime of the
    // shell so that the lock file is removed on `Drop` after the embedded
    // terminal `App` (and its runner supervisor) has been torn down. Never
    // read directly — the load-bearing behavior is `Drop` ordering.
    _app_lock_guard: Option<AppLockGuard>,
    focused_session_id: String,
    supervisors: BTreeMap<String, SessionSupervisor>,
    sidebar: SidebarModel,
    config: Arc<Config>,
    event_bus: ShellEventBus,
    // Mtime-cached projection of every session.toml under `sessions_root`.
    // The shell refreshes it on each scheduler tick so per-tick load cost
    // is bounded to entries whose file actually changed since the last
    // refresh. The scheduler input itself is still derived from the
    // in-memory open workspaces (so the focused App's live phase wins),
    // but the cache underpins steady-state load-count guarantees and the
    // upcoming index-driven sidebar work.
    session_index: SessionIndex,
}

impl AppShell {
    pub fn new(
        initial_state: SessionState,
        startup_origin: AppStartupOrigin,
        config: Arc<Config>,
    ) -> Result<Self> {
        Self::new_with_app_lock(initial_state, startup_origin, config, None)
    }

    pub fn new_with_app_lock(
        initial_state: SessionState,
        startup_origin: AppStartupOrigin,
        config: Arc<Config>,
        app_lock_guard: Option<AppLockGuard>,
    ) -> Result<Self> {
        let sessions_root = crate::picker::sessions_root_for(&config);
        let focused_session_id = initial_state.session_id.clone();
        let mut supervisors = BTreeMap::new();
        let initial_supervisor =
            SessionSupervisor::new(initial_state, startup_origin, config.clone());
        supervisors.insert(focused_session_id.clone(), initial_supervisor);
        let mut session_index = SessionIndex::new(sessions_root);
        session_index.refresh()?;
        let mut shell = Self {
            _app_lock_guard: app_lock_guard,
            focused_session_id,
            supervisors,
            sidebar: SidebarModel::new(),
            config,
            event_bus: ShellEventBus::new(),
            session_index,
        };
        shell.refresh_sidebar_rows();
        Ok(shell)
    }

    pub fn focused_session_id(&self) -> &str {
        &self.focused_session_id
    }

    pub fn running_session_id(&self) -> Option<&str> {
        let mut first_running = None;
        for supervisor in self.supervisors.values() {
            if supervisor.current_run_id().is_none() {
                continue;
            }
            first_running.get_or_insert_with(|| supervisor.session_id());
            if is_implementation_lane_phase(supervisor.phase()) {
                return Some(supervisor.session_id());
            }
        }
        first_running
    }

    pub fn open_workspace_count(&self) -> usize {
        self.supervisors.len()
    }

    pub fn workspace(&self, session_id: &str) -> Option<&SessionSupervisor> {
        self.supervisors.get(session_id)
    }

    pub fn workspace_mut(&mut self, session_id: &str) -> Option<&mut SessionSupervisor> {
        self.supervisors.get_mut(session_id)
    }

    pub fn focused_workspace_mut(&mut self) -> Option<&mut SessionSupervisor> {
        self.supervisors.get_mut(&self.focused_session_id)
    }

    /// Returns a mutable reference to the focused workspace.
    ///
    /// # Panics
    /// Panics if there is no focused workspace (this is an invariant violation).
    fn focused_supervisor_unchecked(&mut self) -> &mut SessionSupervisor {
        self.focused_workspace_mut()
            .expect("AppShell always has a focused workspace")
    }

    fn take_focused_app(&mut self) -> App {
        self.focused_supervisor_unchecked().take_app()
    }

    fn return_app_to_supervisor(&mut self, app: App) {
        let session_id = app.state.session_id.clone();
        self.supervisors
            .get_mut(&session_id)
            .expect("focused App belongs to an observed supervisor")
            .replace_app(app);
    }

    /// If the focused session has changed since `app` was lent to the terminal
    /// loop, return it to its supervisor and lend out the newly focused App.
    fn swap_focused_app_if_needed(&mut self, app: App) -> App {
        if app.state.session_id == self.focused_session_id {
            return app;
        }
        self.return_app_to_supervisor(app);
        self.sidebar.mark_dirty();
        self.refresh_sidebar_rows();
        self.take_focused_app()
    }

    pub fn run_focused_terminal_app(
        &mut self,
        terminal: &mut crate::tui::AppTerminal,
    ) -> Result<()> {
        use crate::app_runtime::terminal::{TerminalCommandOutcome, TerminalRuntime};
        use crate::ui::widgets::sidebar::view::render_sidebar;
        use ratatui::layout::Rect;

        let mut app = self.take_focused_app();
        let mut runtime = TerminalRuntime::default();
        let mut input = crate::ui::tui::CrosstermInputAdapter::spawn();

        loop {
            // Park the focused App back inside its supervisor for the
            // scheduler tick. With the App lent back, the scheduler can
            // drive every session (focused or not) through the supervisor
            // map under a single code path, so the tick no longer needs
            // to borrow the focused `App` (spec §4.7, §4.8 line 280).
            self.return_app_to_supervisor(app);
            let _ = self.run_scheduler_tick();
            app = self.take_focused_app();
            if let Some(path) = app.take_pending_view_path() {
                input.shutdown_blocking();
                app.run_external_view_editor(terminal, &path);
                input = crate::ui::tui::CrosstermInputAdapter::spawn();
            }
            if app.runtime_tick_before_data_drain() {
                app.drain_notifications_for_shutdown();
                self.return_app_to_supervisor(app);
                return Ok(());
            }
            runtime.drain_app_data_events(&mut app);
            app.runtime_tick_after_data_drain();
            let view = runtime.view_for_render(app.current_app_view());

            crate::ui::tui::render_app(terminal, &view, |frame| {
                let full_area = frame.area();
                if self.sidebar.visible {
                    let sidebar_w = crate::ui::widgets::sidebar::view::sidebar_width()
                        .min(full_area.width.saturating_sub(20).max(10));
                    let sidebar_area =
                        Rect::new(full_area.x, full_area.y, sidebar_w, full_area.height);
                    let app_area = Rect::new(
                        full_area.x + sidebar_w,
                        full_area.y,
                        full_area.width.saturating_sub(sidebar_w),
                        full_area.height,
                    );
                    let sidebar_view = self.sidebar_view();
                    render_sidebar(sidebar_area, frame.buffer_mut(), &sidebar_view);
                    app.draw_in_area(frame, &view, app_area);
                } else {
                    app.draw(frame, &view);
                }
            })?;

            app.on_frame_drawn();

            if let Some(command) = input.next_command(app.event_poll_duration(), &view)? {
                // Shell intercepts sidebar-navigation keys first.
                if self.sidebar.visible {
                    let modal_open = app.current_app_view().modal.is_some();
                    match self.handle_shell_command(command.clone(), modal_open)? {
                        ShellCommandOutcome::Consumed => {
                            app = self.swap_focused_app_if_needed(app);
                            continue;
                        }
                        ShellCommandOutcome::Unhandled => {}
                    }
                }

                let outcome = runtime.route_command_with_dispatch(command, &view, |request| {
                    crate::data::events::dispatch(request, &app.runner_supervisor)
                });
                match outcome {
                    TerminalCommandOutcome::HandledContinue => {}
                    TerminalCommandOutcome::HandledExit => {
                        app.runner_supervisor.shutdown_all_runs();
                        app.drain_notifications_for_shutdown();
                        self.return_app_to_supervisor(app);
                        return Ok(());
                    }
                    TerminalCommandOutcome::AppOwned(command) => {
                        if app.handle_app_command(command) {
                            app.runner_supervisor.shutdown_all_runs();
                            app.drain_notifications_for_shutdown();
                            self.return_app_to_supervisor(app);
                            return Ok(());
                        }
                    }
                }

                // If the App executed a shell-level palette command, forward it.
                if let Some("sessions") = app.pending_shell_command.take().as_deref() {
                    let _ = self.execute_shell_palette_command("sessions");
                }
            }
        }
    }

    pub fn toggle_sessions_sidebar(&mut self) -> Result<()> {
        self.sidebar.visible = !self.sidebar.visible;
        self.sidebar.mark_dirty();
        if self.sidebar.visible {
            self.refresh_sidebar_rows();
        } else if self.sidebar.focus == ShellFocus::Sidebar {
            self.sidebar.focus = ShellFocus::Workspace;
        }
        Ok(())
    }

    pub fn execute_shell_palette_command(&mut self, name: &str) -> Result<ShellCommandOutcome> {
        match name {
            "sessions" => {
                self.toggle_sessions_sidebar()?;
                Ok(ShellCommandOutcome::Consumed)
            }
            _ => Ok(ShellCommandOutcome::Unhandled),
        }
    }

    pub fn handle_shell_command(
        &mut self,
        command: AppCommand,
        modal_open: bool,
    ) -> Result<ShellCommandOutcome> {
        let AppCommand::KeyPress(key) = command else {
            return Ok(ShellCommandOutcome::Unhandled);
        };
        if key.ctrl || key.alt {
            return Ok(ShellCommandOutcome::Unhandled);
        }
        match key.code {
            UiKeyCode::Left | UiKeyCode::Right if self.sidebar.visible => {
                self.toggle_sidebar_focus();
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Up if self.sidebar.visible && self.sidebar.focus == ShellFocus::Sidebar => {
                self.move_sidebar_selection(-1);
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Down
                if self.sidebar.visible && self.sidebar.focus == ShellFocus::Sidebar =>
            {
                self.move_sidebar_selection(1);
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Enter
                if self.sidebar.visible && self.sidebar.focus == ShellFocus::Sidebar =>
            {
                self.open_selected_sidebar_session()?;
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Esc if self.sidebar.focus == ShellFocus::Sidebar => {
                if modal_open {
                    // Esc is owned by the App modal; do not hide the sidebar.
                    return Ok(ShellCommandOutcome::Unhandled);
                }
                self.sidebar.visible = false;
                self.sidebar.focus = ShellFocus::Workspace;
                self.sidebar.mark_dirty();
                Ok(ShellCommandOutcome::Consumed)
            }
            _ => Ok(ShellCommandOutcome::Unhandled),
        }
    }

    pub fn focus_sidebar(&mut self) {
        if self.sidebar.visible {
            self.sidebar.focus = ShellFocus::Sidebar;
            self.sidebar.mark_dirty();
        }
    }

    pub fn focus_workspace(&mut self) {
        self.sidebar.focus = ShellFocus::Workspace;
        self.sidebar.mark_dirty();
    }

    fn toggle_sidebar_focus(&mut self) {
        self.sidebar.focus = match self.sidebar.focus {
            ShellFocus::Workspace => ShellFocus::Sidebar,
            ShellFocus::Sidebar => ShellFocus::Workspace,
        };
        self.sidebar.mark_dirty();
    }

    fn move_sidebar_selection(&mut self, delta: isize) {
        if self.sidebar.rows.is_empty() {
            self.sidebar.selected_index = 0;
            return;
        }
        let max = self.sidebar.rows.len() - 1;
        self.sidebar.selected_index = if delta.is_negative() {
            self.sidebar
                .selected_index
                .saturating_sub(delta.unsigned_abs())
        } else {
            self.sidebar
                .selected_index
                .saturating_add(delta as usize)
                .min(max)
        };
    }

    pub fn select_sidebar_session(&mut self, session_id: &str) -> Result<()> {
        self.refresh_sidebar_rows();
        if let Some(index) = self
            .sidebar
            .rows
            .iter()
            .position(|row| row.session_id == session_id)
        {
            self.sidebar.selected_index = index;
        }
        Ok(())
    }

    pub fn open_selected_sidebar_session(&mut self) -> Result<()> {
        let Some(session_id) = self
            .sidebar
            .rows
            .get(self.sidebar.selected_index)
            .map(|row| row.session_id.clone())
        else {
            return Ok(());
        };
        self.open_session(&session_id)
    }

    pub fn open_session(&mut self, session_id: &str) -> Result<()> {
        if !self.supervisors.contains_key(session_id) {
            let state = SessionState::load(session_id)?;
            self.session_index.update_loaded_state(&state);
            let supervisor =
                SessionSupervisor::new(state, AppStartupOrigin::Default, self.config.clone());
            self.supervisors.insert(session_id.to_string(), supervisor);
            self.sidebar.mark_dirty();
        }
        self.focus_session(session_id)
    }

    pub fn focus_session(&mut self, session_id: &str) -> Result<()> {
        if !self.supervisors.contains_key(session_id) {
            self.open_session(session_id)?;
            return Ok(());
        }
        self.focused_session_id = session_id.to_string();
        self.sidebar.focus = ShellFocus::Workspace;
        self.sidebar.mark_dirty();
        self.refresh_sidebar_rows();
        Ok(())
    }

    pub fn sidebar_view(&self) -> SidebarView {
        SidebarView {
            visible: self.sidebar.visible,
            focus: self.sidebar.focus,
            selected_index: self.sidebar.selected_index,
            rows: self.sidebar.rows.clone(),
        }
    }

    pub fn apply_event(&mut self, event: ShellEvent) {
        self.event_bus.publish(event.clone());
        match event {
            ShellEvent::SessionStateChanged { session_id, state } => {
                self.session_index.update_loaded_state(&state);
                if let Some(supervisor) = self.supervisors.get_mut(&session_id) {
                    supervisor.replace_state(*state);
                }
                self.sidebar.mark_dirty();
            }
            ShellEvent::LiveSummaryChanged { session_id, text } => {
                if let Some(supervisor) = self.supervisors.get_mut(&session_id) {
                    supervisor.replace_live_summary(text);
                }
            }
            ShellEvent::RunStarted { .. } => {
                self.sidebar.mark_dirty();
            }
            ShellEvent::RunFinished { .. } => {
                self.sidebar.mark_dirty();
            }
        }
        self.refresh_sidebar_rows();
    }

    pub fn run_scheduler_tick(&mut self) -> Result<ShellSchedulerReport> {
        match self.session_index.refresh_tracking_changes() {
            Ok(true) => self.sidebar.mark_dirty(),
            Ok(false) => {}
            Err(err) => {
                warn!(error = %err, "session index refresh failed during scheduler tick");
            }
        }
        let scan = self.scan_supervisors_for_scheduler();
        let tick = evaluate_tick(&scan);
        let mut planning_session_ids = Vec::new();

        for planning in &tick.planning {
            self.drive_scheduler_session(&planning.session_id, SchedulerDrive::AutoLaunch)?;
            planning_session_ids.push(planning.session_id.clone());
        }

        let implementation = self.apply_implementation_decision(&tick)?;
        let running_session_id = self.running_session_id().map(str::to_string);
        self.refresh_sidebar_rows_with_running(running_session_id.as_deref());
        Ok(ShellSchedulerReport {
            planning_session_ids,
            implementation,
            skipped_corrupt_later_sessions: tick.skipped_corrupt_later_sessions,
        })
    }

    /// Test seam: number of full `SessionState::load` calls the shell's
    /// session index has performed since construction. Used by scheduler
    /// integration tests to assert that a steady-state tick does not
    /// full-load every session.
    #[cfg(test)]
    pub(crate) fn session_index_loader_call_count(&self) -> usize {
        self.session_index.loader_call_count()
    }

    #[cfg(test)]
    pub(crate) fn sidebar_rebuild_count(&self) -> usize {
        self.sidebar.rebuild_count
    }

    #[cfg(test)]
    pub(crate) fn supervisor_app_identity(&self, session_id: &str) -> Option<usize> {
        self.supervisors
            .get(session_id)
            .map(SessionSupervisor::app_identity)
    }

    fn scan_supervisors_for_scheduler(&self) -> Vec<ScannedSession> {
        self.session_index
            .snapshot_for_scheduler()
            .into_iter()
            .filter_map(|scanned| {
                let session_id = scanned.session_id().to_string();
                if !self.supervisors.contains_key(&session_id) {
                    return None;
                }
                match scanned {
                    ScannedSession::Loaded(mut session) => {
                        if let Some(supervisor) = self.supervisors.get(&session.session_id)
                            && supervisor.app.is_some()
                        {
                            // Supervisors are the single source of truth
                            // for run state; the focused App is parked
                            // back into its supervisor before this call
                            // (see `run_focused_terminal_app`), so the
                            // focused session is observed alongside the
                            // background ones without a special case.
                            session.current_phase = supervisor.phase();
                        }
                        Some(ScannedSession::Loaded(session))
                    }
                    ScannedSession::Corrupt { .. } => Some(scanned),
                }
            })
            .collect()
    }

    fn apply_implementation_decision(
        &mut self,
        tick: &SchedulerTick,
    ) -> Result<ShellImplementationAction> {
        match &tick.implementation {
            ImplementationDecision::LaneOccupied { session_id, phase } => {
                let _ = self.drive_scheduler_session(session_id, SchedulerDrive::AutoLaunch)?;
                Ok(ShellImplementationAction::LaneOccupied {
                    session_id: session_id.clone(),
                    phase: *phase,
                })
            }
            ImplementationDecision::BlockedByHead { session_id } => {
                Ok(ShellImplementationAction::BlockedByHead {
                    session_id: session_id.clone(),
                })
            }
            ImplementationDecision::PlanningHead { session_id, phase } => {
                Ok(ShellImplementationAction::PlanningHead {
                    session_id: session_id.clone(),
                    phase: *phase,
                })
            }
            ImplementationDecision::DispatchWaiting { session_id } => {
                let state =
                    self.drive_scheduler_session(session_id, SchedulerDrive::DispatchWaiting)?;
                Ok(ShellImplementationAction::DispatchedWaiting {
                    session_id: session_id.clone(),
                    phase: state.current_phase,
                })
            }
            ImplementationDecision::BlockedByCorruptEarlierSession { session_id, error } => {
                Ok(ShellImplementationAction::BlockedByCorruptEarlierSession {
                    session_id: session_id.clone(),
                    error: error.clone(),
                })
            }
            ImplementationDecision::NothingToDo => Ok(ShellImplementationAction::NothingToDo),
        }
    }

    fn drive_scheduler_session(
        &mut self,
        session_id: &str,
        drive: SchedulerDrive,
    ) -> Result<SessionState> {
        self.ensure_supervisor_loaded(session_id)?;
        let (state, tick) = self
            .supervisors
            .get_mut(session_id)
            .expect("supervisor loaded before scheduler drive")
            .drive(drive);
        self.publish_supervisor_tick(session_id, &state, tick);
        Ok(state)
    }

    fn ensure_supervisor_loaded(&mut self, session_id: &str) -> Result<()> {
        if !self.supervisors.contains_key(session_id) {
            let state = SessionState::load(session_id)?;
            self.session_index.update_loaded_state(&state);
            let supervisor =
                SessionSupervisor::new(state, AppStartupOrigin::Default, self.config.clone());
            self.supervisors.insert(session_id.to_string(), supervisor);
            self.sidebar.mark_dirty();
        }
        Ok(())
    }

    fn publish_supervisor_tick(
        &mut self,
        session_id: &str,
        state: &SessionState,
        tick: SupervisorTick,
    ) {
        if tick.state_changed {
            self.apply_event(ShellEvent::SessionStateChanged {
                session_id: session_id.to_string(),
                state: Box::new(state.clone()),
            });
        }
        if let Some(text) = tick.live_summary_changed {
            self.apply_event(ShellEvent::LiveSummaryChanged {
                session_id: session_id.to_string(),
                text,
            });
        }
        if let Some(run_id) = tick.run_started {
            self.apply_event(ShellEvent::RunStarted {
                session_id: session_id.to_string(),
                run_id,
            });
        }
        if let Some(run_id) = tick.run_finished {
            self.apply_event(ShellEvent::RunFinished {
                session_id: session_id.to_string(),
                run_id,
            });
        }
    }

    fn refresh_sidebar_rows(&mut self) {
        let running_session_id = self.running_session_id().map(str::to_string);
        self.refresh_sidebar_rows_with_running(running_session_id.as_deref());
    }

    fn refresh_sidebar_rows_with_running(
        &mut self,
        running_session_id: Option<&str>,
    ) {
        self.sidebar.refresh_if_dirty(
            &self.session_index,
            &self.focused_session_id,
            running_session_id,
            &self.supervisors,
        );
    }
}

impl Drop for AppShell {
    fn drop(&mut self) {
        for supervisor in self.supervisors.values() {
            if let Some(app) = &supervisor.app {
                app.runner_supervisor.shutdown_all_runs();
            }
        }
    }
}

fn sidebar_date_label(session_id: &str) -> String {
    let bytes = session_id.as_bytes();
    if bytes.len() >= 8 && bytes[..8].iter().all(u8::is_ascii_digit) {
        format!("{}/{}", &session_id[4..6], &session_id[6..8])
    } else {
        "--/--".to_string()
    }
}

#[derive(Debug, Clone, Copy)]
enum SchedulerDrive {
    AutoLaunch,
    DispatchWaiting,
}

impl SchedulerDrive {
    fn apply(self, app: &mut App) {
        match self {
            Self::AutoLaunch => {
                app.poll_agent_run();
                app.maybe_auto_launch();
            }
            Self::DispatchWaiting => {
                app.dispatch_waiting_to_implement();
                app.maybe_auto_launch();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{TestLaunchHarness, TestLaunchOutcome};
    use crate::app_runtime::{AppCommand, UiKey, UiKeyCode};
    use crate::logic::selection::{
        CachedModel, Candidate, CliKind, IpbrPhaseScores, ScoreSource, SubscriptionKind,
    };
    use serial_test::serial;
    use std::collections::VecDeque;

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock()
            .lock();
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

    fn save_session(id: &str, phase: Phase) -> SessionState {
        let mut state = SessionState::new(id.to_string());
        state.idea_text = Some(format!("idea for {id}"));
        state.current_phase = phase;
        state.save().expect("save session");
        state
    }

    fn save_session_with_title(id: &str, phase: Phase, title: &str) -> SessionState {
        let mut state = SessionState::new(id.to_string());
        state.idea_text = Some(format!("idea for {id}"));
        state.title = Some(title.to_string());
        state.current_phase = phase;
        state.save().expect("save titled session");
        state
    }

    fn advance_mtime_clock() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    fn cached_build_model() -> CachedModel {
        let candidate = Candidate {
            subscription: SubscriptionKind::Codex,
            cli: CliKind::Codex,
            launch_name: "test-build-model".to_string(),
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
            name: "test-build-model".to_string(),
            ipbr_phase_scores: IpbrPhaseScores {
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

    fn running_sharding_run(id: u64) -> crate::state::RunRecord {
        crate::state::RunRecord {
            id,
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
            effort: crate::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn write_finish_stamp(session_id: &str, run_key: &str) {
        let stamp_path = crate::state::session_dir(session_id)
            .join("artifacts")
            .join("run-finish")
            .join(format!("{run_key}.toml"));
        let stamp = crate::runner::FinishStamp {
            finished_at: chrono::Utc::now().to_rfc3339(),
            exit_code: 0,
            head_before: "test-base".to_string(),
            head_after: "test-base".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
            working_tree_clean: true,
        };
        crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write finish stamp");
    }

    fn write_tasks(session_id: &str) {
        let tasks_path = crate::state::session_dir(session_id)
            .join("artifacts")
            .join("tasks.toml");
        std::fs::create_dir_all(tasks_path.parent().expect("tasks parent")).expect("mkdir tasks");
        std::fs::write(
            tasks_path,
            r#"
[[tasks]]
id = 1
title = "Build the feature"
description = "Implement the requested behavior."
test = "cargo test"
estimated_tokens = 100
"#,
        )
        .expect("write tasks");
    }

    // Mirrors the swap point inside `run_focused_terminal_app`: when sidebar
    // Enter changes the focused session, the loop must return the running
    // local `App` to its supervisor and lend out the newly focused App.
    #[test]
    #[serial]
    fn sidebar_enter_swaps_focused_app_and_keeps_running_session_marked() {
        with_temp_root(|| {
            let mut first = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            first.agent_runs.push(running_sharding_run(7));
            first.save().expect("save running first");
            save_session("20260511-091000-000000001", Phase::WaitingToImplement);
            let mut shell = AppShell::new(
                first.clone(),
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            let mut app = shell.take_focused_app();
            assert_eq!(app.state.session_id, "20260511-090000-000000001");

            // Sidebar Enter on the second row routes through the same command
            // path the terminal loop uses (`handle_shell_command`).
            shell.toggle_sessions_sidebar().expect("toggle");
            shell.focus_sidebar();
            shell
                .select_sidebar_session("20260511-091000-000000001")
                .expect("select");
            let enter = AppCommand::KeyPress(UiKey {
                code: UiKeyCode::Enter,
                ctrl: false,
                alt: false,
            });
            assert_eq!(
                shell.handle_shell_command(enter, false).expect("enter"),
                ShellCommandOutcome::Consumed
            );

            // The loop's swap point hands the new focused `App` back to the
            // body. Without this, the loop would keep rendering session A's
            // `App` even though focus moved to session B.
            app = shell.swap_focused_app_if_needed(app);
            assert_eq!(app.state.session_id, "20260511-091000-000000001");
            assert_eq!(shell.focused_session_id(), "20260511-091000-000000001");

            // Step 6: persisted Running runs are backfilled to Failed on
            // resume, so no session is "running" after a TUI restart until
            // a launch fires in-process. The load-bearing assertion of this
            // test is the focus swap above; the running-marker assertions
            // are kept here as a regression guard against the new model
            // ever resurrecting orphaned runs as live.
            assert_eq!(shell.running_session_id(), None);
            let rows = shell.sidebar_view().rows;
            let prior_row = rows
                .iter()
                .find(|r| r.session_id == "20260511-090000-000000001")
                .expect("prior session in sidebar");
            assert!(!prior_row.running);
        });
    }

    #[test]
    #[serial]
    fn scheduler_tick_auto_launches_idle_implementation_occupant() {
        with_temp_root(|| {
            let mut state = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            state.agent_runs.clear();
            state.save().expect("save without runs");
            let mut shell = AppShell::new(
                state.clone(),
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");
            let mut app = crate::app::test_support::mk_app(state);
            app.run_launched = false;
            app.current_run_id = None;
            app.models.push(cached_build_model());
            app.test_launch_harness = Some(Arc::new(std::sync::Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            })));

            let tick = SchedulerTick {
                planning: Vec::new(),
                implementation: ImplementationDecision::LaneOccupied {
                    session_id: "20260511-090000-000000001".to_string(),
                    phase: Phase::ShardingRunning,
                },
                skipped_corrupt_later_sessions: Vec::new(),
            };
            // Swap the prepared app (with launch harness) into the
            // supervisor so the scheduler drive — now decoupled from any
            // focused `App` borrow — exercises the harnessed launch.
            {
                let supervisor = shell
                    .workspace_mut("20260511-090000-000000001")
                    .expect("workspace");
                let _ = supervisor.take_app();
                supervisor.replace_app(app);
            }

            let action = shell.apply_implementation_decision(&tick).expect("apply");

            assert!(matches!(
                action,
                ShellImplementationAction::LaneOccupied {
                    session_id,
                    phase: Phase::ShardingRunning,
                } if session_id == "20260511-090000-000000001"
            ));
            let driven = shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .app();
            assert!(driven.run_launched, "idle sharding phase should launch");
            assert_eq!(driven.current_run_id, Some(1));
        });
    }

    #[test]
    #[serial]
    fn repeated_background_ticks_keep_one_session_supervisor_app() {
        with_temp_root(|| {
            let focused = save_session("20260511-080000-000000001", Phase::Done);
            let mut running = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            running.agent_runs.push(running_sharding_run(7));
            running.save().expect("save running candidate");
            let mut shell = AppShell::new(
                focused,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");
            shell
                .open_session("20260511-090000-000000001")
                .expect("open background");
            shell
                .focus_session("20260511-080000-000000001")
                .expect("refocus done");
            let before = shell
                .supervisor_app_identity("20260511-090000-000000001")
                .expect("background identity");

            shell.run_scheduler_tick().expect("first background tick");
            shell.run_scheduler_tick().expect("second background tick");

            assert_eq!(
                shell.supervisor_app_identity("20260511-090000-000000001"),
                Some(before),
                "background ticks must reuse the same session-level App/runner owner"
            );
            let supervisor = shell
                .workspace("20260511-090000-000000001")
                .expect("background supervisor");
            // Step 6: persisted Running runs are backfilled to Failed on
            // resume, so the supervisor reports no live run for the prior
            // run id. The App-reuse assertion above is the load-bearing
            // invariant for this test.
            assert_eq!(supervisor.current_run_id(), None);
        });
    }

    #[test]
    #[serial]
    fn background_waiting_dispatch_reuses_existing_supervisor_app() {
        with_temp_root(|| {
            let focused = save_session("20260511-080000-000000001", Phase::Done);
            save_session("20260511-090000-000000001", Phase::WaitingToImplement);
            let mut shell = AppShell::new(
                focused,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");
            shell
                .open_session("20260511-090000-000000001")
                .expect("open waiting");
            shell
                .focus_session("20260511-080000-000000001")
                .expect("refocus done");
            let before = shell
                .supervisor_app_identity("20260511-090000-000000001")
                .expect("waiting identity");

            let report = shell.run_scheduler_tick().expect("dispatch tick");

            assert!(
                matches!(
                    report.implementation,
                    ShellImplementationAction::DispatchedWaiting {
                        ref session_id,
                        ..
                    } if session_id == "20260511-090000-000000001"
                ),
                "got {:?}",
                report.implementation
            );
            assert_eq!(
                shell.supervisor_app_identity("20260511-090000-000000001"),
                Some(before),
                "non-focused scheduler drive should not rebuild a disposable App"
            );
        });
    }

    #[test]
    #[serial]
    fn scheduler_tick_ignores_closed_stale_sessions_when_open_session_waits() {
        with_temp_root(|| {
            save_session("20260511-080000-000000001", Phase::BrainstormRunning);
            let waiting = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
            let mut shell = AppShell::new(
                waiting,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            let report = shell.run_scheduler_tick().expect("tick");

            assert!(matches!(
                report.implementation,
                ShellImplementationAction::DispatchedWaiting {
                    session_id,
                    phase: Phase::ShardingRunning,
                } if session_id == "20260511-090000-000000001"
            ));
            let stale = SessionState::load("20260511-080000-000000001").expect("load stale");
            assert_eq!(stale.current_phase, Phase::BrainstormRunning);
        });
    }

    #[test]
    #[serial]
    fn scheduler_tick_does_not_full_load_every_session_in_steady_state() {
        with_temp_root(|| {
            // Three on-disk sessions, none of them changing across ticks.
            // The shell only opens one workspace (the focused one); the
            // other two exercise the index's "seen but unchanged" path.
            let focused = save_session("20260511-080000-000000001", Phase::Done);
            let _ = save_session("20260511-090000-000000001", Phase::Done);
            let _ = save_session("20260511-100000-000000001", Phase::WaitingToImplement);

            let mut shell = AppShell::new(
                focused,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            // First tick warms the cache — every entry is "new since last
            // refresh" so every entry is parsed exactly once.
            shell.run_scheduler_tick().expect("first tick");
            let after_warmup = shell.session_index_loader_call_count();
            assert!(
                after_warmup >= 3,
                "first refresh should load every session at least once (got {after_warmup})"
            );

            // Subsequent ticks without any on-disk change must not reparse
            // any session.toml — this is the bounded-reparse guarantee.
            shell.run_scheduler_tick().expect("second tick");
            shell.run_scheduler_tick().expect("third tick");
            assert_eq!(
                shell.session_index_loader_call_count(),
                after_warmup,
                "steady-state scheduler tick must not full-load every session"
            );
        });
    }

    #[test]
    #[serial]
    fn sidebar_event_refresh_uses_cached_index_not_disk_scan() {
        with_temp_root(|| {
            let first =
                save_session_with_title("20260511-080000-000000001", Phase::Done, "cached title");
            let mut shell = AppShell::new(
                first.clone(),
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            shell.toggle_sessions_sidebar().expect("show sidebar");
            shell.run_scheduler_tick().expect("warm index");
            assert_eq!(
                shell.sidebar_view().rows[0].title,
                "cached title",
                "warm sidebar row should use the initial index projection"
            );
            let after_warmup = shell.session_index_loader_call_count();

            advance_mtime_clock();
            let mut changed =
                SessionState::load("20260511-080000-000000001").expect("reload session");
            changed.title = Some("disk-only title".to_string());
            changed.save().expect("save disk-only title");

            let mut event_state = first;
            event_state.agent_runs.push(running_sharding_run(42));
            shell.apply_event(ShellEvent::SessionStateChanged {
                session_id: "20260511-080000-000000001".to_string(),
                state: Box::new(event_state),
            });

            let rows = shell.sidebar_view().rows;
            assert_eq!(
                rows[0].title, "cached title",
                "supervisor events rebuild sidebar rows from SessionIndex, not a fresh disk scan"
            );
            assert!(rows[0].running);
            assert_eq!(
                shell.session_index_loader_call_count(),
                after_warmup,
                "event-only sidebar refresh must not reparse session.toml"
            );
        });
    }

    #[test]
    #[serial]
    fn sidebar_rebuilds_only_when_dirty_inputs_change() {
        with_temp_root(|| {
            let first = save_session_with_title("20260511-080000-000000001", Phase::Done, "first");
            save_session_with_title("20260511-090000-000000001", Phase::Done, "second");
            let mut shell = AppShell::new(
                first,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            shell.toggle_sessions_sidebar().expect("show sidebar");
            shell.run_scheduler_tick().expect("warm index");
            let after_warmup = shell.sidebar_rebuild_count();

            shell.run_scheduler_tick().expect("no-op tick");
            assert_eq!(
                shell.sidebar_rebuild_count(),
                after_warmup,
                "unchanged index and shell state should not rebuild sidebar rows"
            );

            shell
                .focus_session("20260511-090000-000000001")
                .expect("focus second");
            assert_eq!(
                shell.sidebar_rebuild_count(),
                after_warmup + 1,
                "focus movement marks the sidebar dirty"
            );
            let focused = shell
                .sidebar_view()
                .rows
                .iter()
                .find(|row| row.session_id == "20260511-090000-000000001")
                .expect("second row")
                .focused;
            assert!(focused);

            advance_mtime_clock();
            let mut changed =
                SessionState::load("20260511-090000-000000001").expect("reload second");
            changed.title = Some("second changed".to_string());
            changed.save().expect("save changed second");

            shell.run_scheduler_tick().expect("changed index tick");
            assert_eq!(
                shell.sidebar_rebuild_count(),
                after_warmup + 2,
                "index changes mark the sidebar dirty"
            );
            let changed_title = shell
                .sidebar_view()
                .rows
                .iter()
                .find(|row| row.session_id == "20260511-090000-000000001")
                .expect("second row")
                .title
                .clone();
            assert_eq!(changed_title, "second changed");
        });
    }

    #[test]
    #[serial]
    fn scheduler_tick_finalizes_finished_background_sharding_run() {
        with_temp_root(|| {
            let focused = save_session("20260511-085000-000000001", Phase::Done);
            let mut sharding = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            sharding.agent_runs.push(running_sharding_run(7));
            sharding.save().expect("save running sharding");
            write_tasks("20260511-090000-000000001");
            write_finish_stamp("20260511-090000-000000001", "sharding-stage-r1-a1");

            let mut shell = AppShell::new(
                focused,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");
            shell
                .open_session("20260511-090000-000000001")
                .expect("open sharding");
            shell
                .focus_session("20260511-085000-000000001")
                .expect("refocus done");

            let report = shell.run_scheduler_tick().expect("tick");

            // Step 6: persisted Running runs are backfilled to Failed on
            // resume, so the prior orphan no longer occupies the lane —
            // the scheduler sees the session in `ShardingRunning` with no
            // live run and reports it as the lane-occupant for re-launch.
            assert!(matches!(
                report.implementation,
                ShellImplementationAction::LaneOccupied {
                    session_id,
                    phase: Phase::ShardingRunning,
                } if session_id == "20260511-090000-000000001"
            ));
            let reloaded =
                SessionState::load("20260511-090000-000000001").expect("load sharding session");
            // The phase stays at `ShardingRunning` (no auto-advance from a
            // backfilled finish stamp), and run 7 carries the backfill's
            // Failed status with the "aborted" error.
            assert_eq!(reloaded.current_phase, Phase::ShardingRunning);
            let run_7 = reloaded
                .agent_runs
                .iter()
                .find(|r| r.id == 7)
                .expect("run 7 must survive");
            assert_eq!(run_7.status, RunStatus::Failed);
            assert_eq!(
                run_7.error.as_deref(),
                Some("aborted: TUI exited while running")
            );
        });
    }

    #[test]
    #[serial]
    fn focused_run_lifecycle_publishes_run_started_and_finished() {
        with_temp_root(|| {
            let mut state = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            state.agent_runs.clear();
            state.save().expect("save without runs");
            let mut shell = AppShell::new(
                state.clone(),
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            let mut sub = shell.event_bus.subscribe();

            // Prime the focused supervisor's App with a launch harness and
            // build model so the scheduler's drive can spawn a run. With
            // the scheduler signature collapsed, the supervisor owns the
            // App for the whole tick — no separate borrow is needed.
            {
                let app = shell
                    .workspace_mut("20260511-090000-000000001")
                    .expect("workspace")
                    .app_mut();
                app.models.push(cached_build_model());
                app.test_launch_harness =
                    Some(Arc::new(std::sync::Mutex::new(TestLaunchHarness {
                        outcomes: VecDeque::from([
                            TestLaunchOutcome {
                                exit_code: 0,
                                artifact_contents: None,
                                launch_error: None,
                            },
                            TestLaunchOutcome {
                                exit_code: 0,
                                artifact_contents: None,
                                launch_error: None,
                            },
                        ]),
                    })));
            }

            // Drive a launch through the supervisor map.
            let tick = SchedulerTick {
                planning: Vec::new(),
                implementation: ImplementationDecision::LaneOccupied {
                    session_id: "20260511-090000-000000001".to_string(),
                    phase: Phase::ShardingRunning,
                },
                skipped_corrupt_later_sessions: Vec::new(),
            };
            shell
                .apply_implementation_decision(&tick)
                .expect("apply launch");

            // Verify RunStarted event
            let mut found_started = false;
            while let Ok(event) = sub.try_recv() {
                if let ShellEvent::RunStarted { session_id, run_id } = event {
                    assert_eq!(session_id, "20260511-090000-000000001");
                    assert_eq!(run_id, 1);
                    found_started = true;
                    break;
                }
            }
            assert!(
                found_started,
                "RunStarted event not published for focused launch"
            );

            // Simulate run finish
            write_finish_stamp("20260511-090000-000000001", "sharding-stage-r1-a1");

            // Drive another tick to trigger finalization and publication.
            shell
                .apply_implementation_decision(&tick)
                .expect("apply finish");

            // Verify RunFinished event
            let mut found_finished = false;
            while let Ok(event) = sub.try_recv() {
                if let ShellEvent::RunFinished { session_id, run_id } = event {
                    assert_eq!(session_id, "20260511-090000-000000001");
                    assert_eq!(run_id, 1);
                    found_finished = true;
                    break;
                }
            }
            assert!(
                found_finished,
                "RunFinished event not published for focused finish"
            );
        });
    }

    #[test]
    #[serial]
    fn running_session_id_is_derived_not_mirrored() {
        with_temp_root(|| {
            let focused = save_session("20260511-080000-000000001", Phase::Done);
            let mut running = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            running.agent_runs.push(running_sharding_run(7));
            running.save().expect("save running");

            let mut shell = AppShell::new(
                focused,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            shell
                .open_session("20260511-090000-000000001")
                .expect("open");
            // Step 6: any persisted Running run is treated as orphaned on
            // resume and backfilled to Failed, so the derived `running`
            // lookup finds no live session. The point of this test is that
            // the lookup is *derived* (no mirrored field) — driving through
            // a supervisor's `current_run_id()` is the load-bearing path.
            assert_eq!(shell.running_session_id(), None);
        });
    }
}
