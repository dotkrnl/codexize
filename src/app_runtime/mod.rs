//! Thin application runtime seam.
//!
//! The event pump still lives behind [`crate::app::App`] today; this module
//! gives later moves a stable canonical path without changing public CLI
//! behavior. It also reifies the runtime/UI seam as two value-typed
//! enums plus a pair of channels:
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
//! the runtime side without touching the terminal — the same shape a
//! future server-mode binary or web frontend will reuse.

pub mod command;
pub mod harness;
pub mod view;

pub use command::AppCommand;
pub use harness::{RuntimeChannels, UiChannels, channel_pair};
pub use view::{AgentRunSummary, AppView, ModalKind, StageId, StatusMessage, StatusSeverity};

pub use crate::app::App;
pub use crate::app::AppStartupOrigin;
