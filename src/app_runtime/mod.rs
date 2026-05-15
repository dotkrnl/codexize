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
//! [`view::AppView`] and [`command::AppCommand`] are intentionally
//! UI-neutral: they carry no `ratatui`/`crossterm` types and no mutable
//! cache handles. [`harness`] proves the seam by wiring a stubbed UI to
//! the runtime side without touching the terminal.
//!
//! The newer [`frontend`] / [`root_view`] modules introduce the
//! spec-pinned `Frontend` trait, `FrontendConnector`, `RootView`, and
//! `RootEvent` shapes. They live side-by-side with `view::AppView` while
//! later tasks migrate per-surface state onto `app_runtime/views/` and
//! split `AppCommand` into per-surface groupings under
//! `app_runtime/commands/`.
pub mod command;
pub mod commands;
pub mod frontend;
pub mod root_view;
pub(crate) mod stages;
pub mod terminal;
pub mod view;
pub mod views;
pub use crate::app::App;
pub use crate::app::AppStartupOrigin;
pub use command::{AppCommand, UiKey, UiKeyCode};
pub use frontend::{Frontend, FrontendConnector, ShutdownSignal, SnapshotHandle};
pub use root_view::{
    RootEvent, RootEventPayload, RootView, SessionId, SessionView, SessionViewDelta, ShellFocus,
    ShellView, ShellViewDelta, SidebarRow,
};
pub use terminal::run_terminal_app;
pub use view::{
    AgentRunSummary, AppView, ModalKind, ModeFlags, StageId, StatusMessage, StatusSeverity,
};
