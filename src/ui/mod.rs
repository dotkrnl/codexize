//! Canonical home for terminal UI, rendering, and UI-only view state.
//!
//! These re-exports preserve existing paths while callers migrate to the
//! explicit UI layer.

pub mod chrome;
pub mod clock;
pub mod focus_caps;
pub mod input_editor;
pub mod preflight;
pub mod tui;

pub use crate::app as runtime;
pub use crate::dashboard;
pub use crate::picker;
pub use crate::smoke;
