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
    ImplementationDecision, ScannedSession, SchedulerSession, SchedulerTick, evaluate_tick,
};
use crate::state::{Message, Phase, RunStatus, SessionState};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

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

pub struct SessionWorkspace {
    state: SessionState,
    startup_origin: AppStartupOrigin,
    messages: Vec<Message>,
    current_run_id: Option<u64>,
    input_buffer: String,
    input_cursor: usize,
    viewport_top: usize,
    split_run_id: Option<u64>,
    live_summary_cached_text: String,
}

impl SessionWorkspace {
    fn new(state: SessionState, startup_origin: AppStartupOrigin) -> Self {
        let current_run_id = state
            .agent_runs
            .iter()
            .find(|run| run.status == RunStatus::Running)
            .map(|run| run.id);
        let messages = SessionState::load_messages(&state.session_id).unwrap_or_default();
        Self {
            state,
            startup_origin,
            messages,
            current_run_id,
            input_buffer: String::new(),
            input_cursor: 0,
            viewport_top: 0,
            split_run_id: None,
            live_summary_cached_text: String::new(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.state.session_id
    }

    pub fn phase(&self) -> Phase {
        self.state.current_phase
    }

    pub fn current_run_id(&self) -> Option<u64> {
        self.current_run_id
    }

    pub fn live_summary_text(&self) -> &str {
        &self.live_summary_cached_text
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn set_ui_probe_state(
        &mut self,
        input_buffer: impl Into<String>,
        viewport_top: usize,
        split_run_id: Option<u64>,
    ) {
        self.input_buffer = input_buffer.into();
        self.input_cursor = self.input_buffer.chars().count();
        self.viewport_top = viewport_top;
        self.split_run_id = split_run_id;
    }

    pub fn ui_probe_state(&self) -> (String, usize, Option<u64>) {
        (
            self.input_buffer.clone(),
            self.viewport_top,
            self.split_run_id,
        )
    }

    fn replace_state(&mut self, state: SessionState) {
        self.current_run_id = state
            .agent_runs
            .iter()
            .find(|run| run.status == RunStatus::Running)
            .map(|run| run.id);
        self.state = state;
    }

    fn replace_live_summary(&mut self, text: String) {
        self.live_summary_cached_text = crate::app::render::sanitize_live_summary(&text);
    }

    fn rebuild_terminal_app(&self, config: Arc<Config>) -> App {
        // Build the terminal App only when the workspace is actually run; lazy
        // sidebar open/focus must not trigger model refreshes or agent launches.
        App::new_with_startup_origin_and_config(self.state.clone(), self.startup_origin, config)
    }

    fn absorb_terminal_app(&mut self, app: App) {
        self.current_run_id = app.current_run_id;
        self.startup_origin = app.startup_origin;
        self.state = app.state;
        self.messages = app.messages;
        self.input_buffer = app.input_buffer;
        self.input_cursor = app.input_cursor;
        self.viewport_top = app.viewport_top;
        self.split_run_id = match app.split_target {
            Some(crate::app::split::SplitTarget::Run(run_id)) => Some(run_id),
            Some(crate::app::split::SplitTarget::Idea) | None => None,
        };
        self.live_summary_cached_text = app.live_summary_cached_text;
    }
}

pub struct AppShell {
    // Project-level single-process lock guard. Held for the lifetime of the
    // shell so that the lock file is removed on `Drop` after the embedded
    // terminal `App` (and its runner supervisor) has been torn down. Never
    // read directly — the load-bearing behavior is `Drop` ordering.
    #[allow(dead_code)]
    app_lock_guard: Option<AppLockGuard>,
    sessions_root: PathBuf,
    focused_session_id: String,
    running_session_id: Option<String>,
    running_run_id: Option<u64>,
    workspaces: BTreeMap<String, SessionWorkspace>,
    sidebar_visible: bool,
    sidebar_focus: ShellFocus,
    sidebar_selected_index: usize,
    sidebar_rows: Vec<SidebarRow>,
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
        let mut workspaces = BTreeMap::new();
        let initial_workspace = SessionWorkspace::new(initial_state, startup_origin);
        let running_run_id = initial_workspace.current_run_id();
        let running_session_id = running_run_id.map(|_| focused_session_id.clone());
        workspaces.insert(focused_session_id.clone(), initial_workspace);
        let session_index = SessionIndex::new(sessions_root.clone());
        let mut shell = Self {
            app_lock_guard,
            sessions_root,
            focused_session_id,
            running_session_id,
            running_run_id,
            workspaces,
            sidebar_visible: false,
            sidebar_focus: ShellFocus::Workspace,
            sidebar_selected_index: 0,
            sidebar_rows: Vec::new(),
            config,
            event_bus: ShellEventBus::new(),
            session_index,
        };
        shell.refresh_sidebar_rows()?;
        Ok(shell)
    }

    pub fn event_bus(&self) -> ShellEventBus {
        self.event_bus.clone()
    }

    pub fn focused_session_id(&self) -> &str {
        &self.focused_session_id
    }

    pub fn running_session_id(&self) -> Option<&str> {
        self.running_session_id.as_deref()
    }

    pub fn open_workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    pub fn workspace(&self, session_id: &str) -> Option<&SessionWorkspace> {
        self.workspaces.get(session_id)
    }

    pub fn workspace_mut(&mut self, session_id: &str) -> Option<&mut SessionWorkspace> {
        self.workspaces.get_mut(session_id)
    }

    pub fn focused_workspace_mut(&mut self) -> Option<&mut SessionWorkspace> {
        self.workspaces.get_mut(&self.focused_session_id)
    }

    /// Returns a mutable reference to the focused workspace.
    ///
    /// # Panics
    /// Panics if there is no focused workspace (this is an invariant violation).
    fn focused_workspace_unchecked(&mut self) -> &mut SessionWorkspace {
        self.focused_workspace_mut()
            .expect("AppShell always has a focused workspace")
    }

    /// If the focused session has changed since `app` was built, absorb the old
    /// app into its workspace and rebuild a fresh `App` for the newly focused
    /// session. Background run tracking on the shell is untouched.
    fn swap_focused_app_if_needed(&mut self, app: App) -> App {
        if app.state.session_id == self.focused_session_id {
            return app;
        }
        let old_session_id = app.state.session_id.clone();
        if let Some(workspace) = self.workspaces.get_mut(&old_session_id) {
            workspace.absorb_terminal_app(app);
        }
        let config = self.config.clone();
        self.focused_workspace_unchecked()
            .rebuild_terminal_app(config)
    }

    pub fn run_focused_terminal_app(
        &mut self,
        terminal: &mut crate::tui::AppTerminal,
    ) -> Result<()> {
        use crate::app_runtime::terminal::{TerminalCommandOutcome, TerminalRuntime};
        use crate::ui::widgets::sidebar::view::render_sidebar;
        use ratatui::layout::Rect;

        let config = self.config.clone();
        let workspace = self.focused_workspace_unchecked();
        let mut app = workspace.rebuild_terminal_app(config);
        let mut runtime = TerminalRuntime::default();
        let mut input = crate::ui::tui::CrosstermInputAdapter::spawn();

        loop {
            let _ = self.run_scheduler_tick_with_focused_app(Some(&mut app));
            if let Some(path) = app.take_pending_view_path() {
                input.shutdown_blocking();
                app.run_external_view_editor(terminal, &path);
                input = crate::ui::tui::CrosstermInputAdapter::spawn();
            }
            if app.runtime_tick_before_data_drain()? {
                app.drain_notifications_for_shutdown();
                self.focused_workspace_unchecked().absorb_terminal_app(app);
                return Ok(());
            }
            runtime.drain_app_data_events(&mut app);
            app.runtime_tick_after_data_drain();
            let view = runtime.view_for_render(app.current_app_view());

            crate::ui::tui::render_app(terminal, &view, |frame| {
                let full_area = frame.area();
                if self.sidebar_visible {
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
                if self.sidebar_visible {
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
                        self.focused_workspace_unchecked().absorb_terminal_app(app);
                        return Ok(());
                    }
                    TerminalCommandOutcome::AppOwned(command) => {
                        if app.handle_app_command(command) {
                            app.runner_supervisor.shutdown_all_runs();
                            app.drain_notifications_for_shutdown();
                            self.focused_workspace_unchecked().absorb_terminal_app(app);
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
        self.sidebar_visible = !self.sidebar_visible;
        if self.sidebar_visible {
            self.refresh_sidebar_rows()?;
        } else if self.sidebar_focus == ShellFocus::Sidebar {
            self.sidebar_focus = ShellFocus::Workspace;
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
            UiKeyCode::Left | UiKeyCode::Right if self.sidebar_visible => {
                self.toggle_sidebar_focus();
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Up if self.sidebar_visible && self.sidebar_focus == ShellFocus::Sidebar => {
                self.move_sidebar_selection(-1);
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Down
                if self.sidebar_visible && self.sidebar_focus == ShellFocus::Sidebar =>
            {
                self.move_sidebar_selection(1);
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Enter
                if self.sidebar_visible && self.sidebar_focus == ShellFocus::Sidebar =>
            {
                self.open_selected_sidebar_session()?;
                Ok(ShellCommandOutcome::Consumed)
            }
            UiKeyCode::Esc if self.sidebar_focus == ShellFocus::Sidebar => {
                if modal_open {
                    // Esc is owned by the App modal; do not hide the sidebar.
                    return Ok(ShellCommandOutcome::Unhandled);
                }
                self.sidebar_visible = false;
                self.sidebar_focus = ShellFocus::Workspace;
                Ok(ShellCommandOutcome::Consumed)
            }
            _ => Ok(ShellCommandOutcome::Unhandled),
        }
    }

    pub fn focus_sidebar(&mut self) {
        if self.sidebar_visible {
            self.sidebar_focus = ShellFocus::Sidebar;
        }
    }

    pub fn focus_workspace(&mut self) {
        self.sidebar_focus = ShellFocus::Workspace;
    }

    fn toggle_sidebar_focus(&mut self) {
        self.sidebar_focus = match self.sidebar_focus {
            ShellFocus::Workspace => ShellFocus::Sidebar,
            ShellFocus::Sidebar => ShellFocus::Workspace,
        };
    }

    fn move_sidebar_selection(&mut self, delta: isize) {
        if self.sidebar_rows.is_empty() {
            self.sidebar_selected_index = 0;
            return;
        }
        let max = self.sidebar_rows.len() - 1;
        self.sidebar_selected_index = if delta.is_negative() {
            self.sidebar_selected_index
                .saturating_sub(delta.unsigned_abs())
        } else {
            self.sidebar_selected_index
                .saturating_add(delta as usize)
                .min(max)
        };
    }

    pub fn select_sidebar_session(&mut self, session_id: &str) -> Result<()> {
        self.refresh_sidebar_rows()?;
        if let Some(index) = self
            .sidebar_rows
            .iter()
            .position(|row| row.session_id == session_id)
        {
            self.sidebar_selected_index = index;
        }
        Ok(())
    }

    pub fn open_selected_sidebar_session(&mut self) -> Result<()> {
        let Some(session_id) = self
            .sidebar_rows
            .get(self.sidebar_selected_index)
            .map(|row| row.session_id.clone())
        else {
            return Ok(());
        };
        self.open_session(&session_id)
    }

    pub fn open_session(&mut self, session_id: &str) -> Result<()> {
        if !self.workspaces.contains_key(session_id) {
            let state = SessionState::load(session_id)?;
            let workspace = SessionWorkspace::new(state, AppStartupOrigin::Default);
            self.workspaces.insert(session_id.to_string(), workspace);
        }
        self.focus_session(session_id)
    }

    pub fn focus_session(&mut self, session_id: &str) -> Result<()> {
        if !self.workspaces.contains_key(session_id) {
            self.open_session(session_id)?;
            return Ok(());
        }
        self.focused_session_id = session_id.to_string();
        self.sidebar_focus = ShellFocus::Workspace;
        self.refresh_sidebar_rows()?;
        Ok(())
    }

    pub fn sidebar_view(&self) -> SidebarView {
        SidebarView {
            visible: self.sidebar_visible,
            focus: self.sidebar_focus,
            selected_index: self.sidebar_selected_index,
            rows: self.sidebar_rows.clone(),
        }
    }

    pub fn apply_event(&mut self, event: ShellEvent) {
        self.event_bus.publish(event.clone());
        match event {
            ShellEvent::SessionStateChanged { session_id, state } => {
                if let Some(workspace) = self.workspaces.get_mut(&session_id) {
                    workspace.replace_state(*state);
                }
            }
            ShellEvent::LiveSummaryChanged { session_id, text } => {
                if let Some(workspace) = self.workspaces.get_mut(&session_id) {
                    workspace.replace_live_summary(text);
                }
            }
            ShellEvent::RunStarted { session_id, run_id } => {
                self.running_session_id = Some(session_id);
                self.running_run_id = Some(run_id);
            }
            ShellEvent::RunFinished { session_id, run_id } => {
                if self.running_session_id.as_deref() == Some(session_id.as_str())
                    && self.running_run_id == Some(run_id)
                {
                    self.running_session_id = None;
                    self.running_run_id = None;
                }
            }
        }
        let _ = self.refresh_sidebar_rows();
    }

    pub fn run_scheduler_tick(&mut self) -> Result<ShellSchedulerReport> {
        self.run_scheduler_tick_with_focused_app(None)
    }

    /// Test seam: number of full `SessionState::load` calls the shell's
    /// session index has performed since construction. Used by scheduler
    /// integration tests to assert that a steady-state tick does not
    /// full-load every session.
    #[cfg(test)]
    pub(crate) fn session_index_loader_call_count(&self) -> usize {
        self.session_index.loader_call_count()
    }

    fn run_scheduler_tick_with_focused_app(
        &mut self,
        mut focused_app: Option<&mut App>,
    ) -> Result<ShellSchedulerReport> {
        // Refresh the cached projection up front so subsequent reads
        // (sidebar refresh below, future supervisor-driven snapshots)
        // share one bounded disk pass per tick. Refresh failure is
        // treated as a soft error: the scheduler still runs against the
        // in-memory workspaces, matching prior behavior where a missing
        // sessions directory was tolerated.
        let _ = self.session_index.refresh();
        let scan = self.scan_open_workspaces_for_scheduler(focused_app.as_deref());
        let tick = evaluate_tick(&scan);
        let mut planning_session_ids = Vec::new();

        for planning in &tick.planning {
            self.drive_scheduler_session(
                &planning.session_id,
                SchedulerDrive::AutoLaunch,
                focused_app.as_deref_mut(),
            )?;
            planning_session_ids.push(planning.session_id.clone());
        }

        let implementation = self.apply_implementation_decision(&tick, focused_app)?;
        self.refresh_sidebar_rows()?;
        Ok(ShellSchedulerReport {
            planning_session_ids,
            implementation,
            skipped_corrupt_later_sessions: tick.skipped_corrupt_later_sessions,
        })
    }

    fn scan_open_workspaces_for_scheduler(&self, focused_app: Option<&App>) -> Vec<ScannedSession> {
        let focused_phase =
            focused_app.map(|app| (app.state.session_id.as_str(), app.state.current_phase));
        self.workspaces
            .iter()
            .map(|(session_id, workspace)| {
                let current_phase = match focused_phase {
                    Some((focused_id, phase)) if focused_id == session_id.as_str() => phase,
                    _ => workspace.phase(),
                };
                ScannedSession::Loaded(SchedulerSession {
                    session_id: session_id.clone(),
                    current_phase,
                })
            })
            .collect()
    }

    fn apply_implementation_decision(
        &mut self,
        tick: &SchedulerTick,
        focused_app: Option<&mut App>,
    ) -> Result<ShellImplementationAction> {
        match &tick.implementation {
            ImplementationDecision::LaneOccupied { session_id, phase } => {
                let _ = self.drive_scheduler_session(
                    session_id,
                    SchedulerDrive::AutoLaunch,
                    focused_app,
                )?;
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
                let state = self.drive_scheduler_session(
                    session_id,
                    SchedulerDrive::DispatchWaiting,
                    focused_app,
                )?;
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
        focused_app: Option<&mut App>,
    ) -> Result<SessionState> {
        if let Some(app) = focused_app
            && app.state.session_id == session_id
        {
            let before_run_id = app.current_run_id;
            drive.apply(app);
            let state = app.state.clone();
            self.publish_scheduler_state(session_id, &state, before_run_id, app.current_run_id);
            return Ok(state);
        }

        self.ensure_workspace_loaded(session_id)?;
        let config = self.config.clone();
        let workspace = self
            .workspaces
            .get(session_id)
            .expect("workspace loaded before scheduler drive");
        let mut app = workspace.rebuild_terminal_app(config);
        // Restore run tracking from the workspace's preserved current_run_id so
        // a background tick cannot re-launch a session that already has an
        // active run. `App::new_with_startup_origin_and_config` recovers this
        // from `state.agent_runs` via `resume_running_runs`, but mirror it
        // explicitly here: the workspace is the in-process source of truth.
        app.current_run_id = workspace.current_run_id();
        app.run_launched = workspace.current_run_id().is_some();
        let before_run_id = app.current_run_id;
        drive.apply(&mut app);
        let state = app.state.clone();
        let after_run_id = app.current_run_id;
        self.workspaces
            .get_mut(session_id)
            .expect("workspace loaded before absorb")
            .absorb_terminal_app(app);
        self.publish_scheduler_state(session_id, &state, before_run_id, after_run_id);
        Ok(state)
    }

    fn ensure_workspace_loaded(&mut self, session_id: &str) -> Result<()> {
        if !self.workspaces.contains_key(session_id) {
            let state = SessionState::load(session_id)?;
            let workspace = SessionWorkspace::new(state, AppStartupOrigin::Default);
            self.workspaces.insert(session_id.to_string(), workspace);
        }
        Ok(())
    }

    fn publish_scheduler_state(
        &mut self,
        session_id: &str,
        state: &SessionState,
        before_run_id: Option<u64>,
        after_run_id: Option<u64>,
    ) {
        self.apply_event(ShellEvent::SessionStateChanged {
            session_id: session_id.to_string(),
            state: Box::new(state.clone()),
        });
        if before_run_id != after_run_id {
            if let Some(run_id) = after_run_id {
                self.apply_event(ShellEvent::RunStarted {
                    session_id: session_id.to_string(),
                    run_id,
                });
            }
            if let Some(run_id) = before_run_id
                && after_run_id.is_none()
            {
                self.apply_event(ShellEvent::RunFinished {
                    session_id: session_id.to_string(),
                    run_id,
                });
            }
        }
    }

    fn refresh_sidebar_rows(&mut self) -> Result<()> {
        let sessions =
            crate::data::picker_io::scan_sessions_by_creation_order(&self.sessions_root)?;
        self.sidebar_rows = sessions
            .into_iter()
            .filter(|entry| entry.current_phase != Phase::Cancelled)
            .map(|entry| {
                let session_id = entry.session_id;
                SidebarRow {
                    focused: session_id == self.focused_session_id,
                    open: self.workspaces.contains_key(&session_id),
                    running: self.running_session_id.as_deref() == Some(session_id.as_str()),
                    date_label: sidebar_date_label(&session_id),
                    title: entry.idea_summary,
                    session_id,
                    phase: entry.current_phase,
                }
            })
            .collect();
        if self.sidebar_selected_index >= self.sidebar_rows.len() {
            self.sidebar_selected_index = self.sidebar_rows.len().saturating_sub(1);
        }
        Ok(())
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
            .lock()
            .unwrap_or_else(|err| err.into_inner());
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
    // Enter changes the focused session, the loop must absorb the running
    // local `App` into the prior workspace, rebuild a fresh `App` for the new
    // focused workspace, and leave shell-level run tracking untouched.
    #[test]
    #[serial]
    fn sidebar_enter_swaps_focused_app_and_keeps_running_session_marked() {
        with_temp_root(|| {
            let first = save_session("20260511-090000-000000001", Phase::ShardingRunning);
            save_session("20260511-091000-000000001", Phase::WaitingToImplement);
            let mut shell = AppShell::new(
                first,
                AppStartupOrigin::Default,
                Arc::new(Config::baked_defaults()),
            )
            .expect("shell");

            shell.apply_event(ShellEvent::RunStarted {
                session_id: "20260511-090000-000000001".into(),
                run_id: 7,
            });

            let config = shell.config.clone();
            let mut app = shell
                .focused_workspace_unchecked()
                .rebuild_terminal_app(config);
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

            // Shell-level run tracking is independent of focus; the prior
            // running session must remain marked running.
            assert_eq!(
                shell.running_session_id(),
                Some("20260511-090000-000000001")
            );
            let rows = shell.sidebar_view().rows;
            let running_row = rows
                .iter()
                .find(|r| r.session_id == "20260511-090000-000000001")
                .expect("running session in sidebar");
            assert!(running_row.running);
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
            let action = shell
                .apply_implementation_decision(&tick, Some(&mut app))
                .expect("apply");

            assert!(matches!(
                action,
                ShellImplementationAction::LaneOccupied {
                    session_id,
                    phase: Phase::ShardingRunning,
                } if session_id == "20260511-090000-000000001"
            ));
            assert!(app.run_launched, "idle sharding phase should launch");
            assert_eq!(app.current_run_id, Some(1));
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

            assert!(matches!(
                report.implementation,
                ShellImplementationAction::LaneOccupied {
                    session_id,
                    phase: Phase::ShardingRunning,
                } if session_id == "20260511-090000-000000001"
            ));
            let reloaded =
                SessionState::load("20260511-090000-000000001").expect("load sharding session");
            assert_eq!(reloaded.current_phase, Phase::ImplementationRound(1));
            assert_eq!(reloaded.agent_runs[0].status, RunStatus::Done);
        });
    }
}
