//! Canonical home for terminal UI, rendering, and UI-only view state.
//!
//! These re-exports preserve existing paths while callers migrate to the
//! explicit UI layer.

pub use crate::app as runtime;
pub use crate::dashboard;
pub use crate::input_editor;
pub use crate::picker;
pub use crate::preflight;
pub use crate::smoke;
pub use crate::tui;
