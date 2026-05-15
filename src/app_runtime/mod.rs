//! Thin application runtime seam.
//!
//! The production terminal event pump now enters through
//! [`terminal::run_terminal_app`]. Terminal polling and orchestration live on
//! this canonical runtime path, with focus-local command handling still owned
//! by [`crate::app::App`]. The module also reifies the runtime/UI seam as two
//! value-typed enums plus a pair of channels:
//!
//! ```text
//! ui  --AppCommand-->  app_runtime  --DataRequest-->  data
//! ui  <--AppView----   app_runtime  <--DataEvent---   data
//!                        |
//!                        +--Decision--> logic (sync, pure call)
//! ```
//!
//! [`AppView`] and [`command::AppCommand`] are intentionally
//! UI-neutral: they carry no `ratatui`/`crossterm` types and no mutable
//! cache handles. [`harness`] proves the seam by wiring a stubbed UI to
//! the runtime side without touching the terminal.
//!
//! The newer [`frontend`] / [`root_view`] modules introduce the
//! spec-pinned `Frontend` trait, `FrontendConnector`, `RootView`, and
//! `RootEvent` shapes. They live side-by-side with `AppView` while
//! later tasks migrate per-surface state onto `app_runtime/views/` and
//! split `AppCommand` into per-surface groupings under
//! `app_runtime/commands/`.
pub mod command;
pub mod commands;
pub mod frontend;
pub mod root_view;
pub mod run;
pub(crate) mod stages;
pub mod terminal;
pub mod views;
pub use crate::app::App;
pub use crate::app::AppStartupOrigin;
pub use command::{AppCommand, UiKey, UiKeyCode};
pub use frontend::{Frontend, FrontendConnector, ShutdownSignal, SnapshotHandle};
pub use root_view::{RootEvent, RootEventPayload, RootView, SessionId, SessionViewDelta, ShellViewDelta};
pub use views::session::SessionView;
pub use views::shell::{ShellFocus, ShellView, SidebarRow};
pub use run::{RuntimePublisher, TerminalFrontend, build_connector, run_frontend};
pub use terminal::run_terminal_app;
pub use views::modal::{ModalKind, StageId};
pub use views::session::{AgentRunSummary, ModeFlags};
pub use views::status_line::{StatusMessage, StatusSeverity};

// Keep AppView for now as it's used by the legacy TUI loop
use crate::logic::pipeline::Stage;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppView {
    pub session_id: Arc<str>,
    pub stage: Stage,
    pub modal: Option<ModalKind>,
    pub status: Option<StatusMessage>,
    pub agent_runs: Arc<[AgentRunSummary]>,
    pub follow_tail: bool,
    pub agent_running: bool,
    pub modes: ModeFlags,
}

impl AppView {
    pub fn empty(session_id: impl Into<Arc<str>>) -> Self {
        Self {
            session_id: session_id.into(),
            stage: Stage::IdeaInput,
            modal: None,
            status: None,
            agent_runs: Arc::from(Vec::<AgentRunSummary>::new()),
            follow_tail: true,
            agent_running: false,
            modes: ModeFlags::default(),
        }
    }
}
