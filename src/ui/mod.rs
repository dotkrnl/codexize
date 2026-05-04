//! Canonical home for terminal UI, rendering, and UI-only view state.
//!
//! These re-exports preserve existing paths while callers migrate to the
//! explicit UI layer.

pub mod chrome;
pub mod clock;
pub mod focus_caps;
pub mod footer;
pub mod input_editor;
pub(crate) mod palette;
pub mod preflight;
pub mod sheet;
pub mod status_line;
pub mod tui;

pub use crate::app as runtime;
pub use crate::dashboard;
pub use crate::picker;
pub use crate::smoke;
