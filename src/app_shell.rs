//! Project-level shell for lazy session workspaces.
//!
//! The existing [`crate::app::App`] remains the focus-local session runtime.
//! `AppShell` is the project owner above it: it keeps the startup-picker
//! selection as the first workspace, tracks focused and running sessions
//! separately, and applies runner/scheduler notifications to already-open
//! workspaces through an in-process event path.

use crate::app::{App, AppStartupOrigin};
use crate::data::app_lock::AppLockGuard;
use crate::data::config::{Config, view::PathsView};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub session_id: String,
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
    fn new(state: SessionState, startup_origin: AppStartupOrigin, _config: Arc<Config>) -> Self {
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
    _app_lock_guard: Option<AppLockGuard>,
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
        let sessions_root = sessions_root_from_config(&config.paths_view(), &config);
        let focused_session_id = initial_state.session_id.clone();
        let mut workspaces = BTreeMap::new();
        let initial_workspace =
            SessionWorkspace::new(initial_state, startup_origin, config.clone());
        let running_run_id = initial_workspace.current_run_id();
        let running_session_id = running_run_id.map(|_| focused_session_id.clone());
        workspaces.insert(focused_session_id.clone(), initial_workspace);
        let mut shell = Self {
            _app_lock_guard: app_lock_guard,
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

    pub fn run_focused_terminal_app(
        &mut self,
        terminal: &mut crate::tui::AppTerminal,
    ) -> Result<()> {
        let config = self.config.clone();
        let workspace = self
            .focused_workspace_mut()
            .expect("AppShell always has a focused workspace");
        let mut app = workspace.rebuild_terminal_app(config);
        let result = crate::app_runtime::run_terminal_app(&mut app, terminal);
        workspace.absorb_terminal_app(app);
        result
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

    pub fn focus_sidebar(&mut self) {
        if self.sidebar_visible {
            self.sidebar_focus = ShellFocus::Sidebar;
        }
    }

    pub fn focus_workspace(&mut self) {
        self.sidebar_focus = ShellFocus::Workspace;
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
            let workspace =
                SessionWorkspace::new(state, AppStartupOrigin::Default, self.config.clone());
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

fn sessions_root_from_config(paths: &PathsView, config: &Config) -> PathBuf {
    if config.paths.sessions_root.is_explicit() {
        paths.sessions_root.clone()
    } else {
        crate::picker::default_sessions_root()
    }
}
