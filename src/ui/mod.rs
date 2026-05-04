//! Canonical home for terminal UI, rendering, and UI-only view state.
//!
//! These re-exports preserve existing paths while callers migrate to the
//! explicit UI layer.

pub mod chat_widget;
pub(crate) mod chat_widget_view_model;
pub mod chrome;
pub mod clock;
pub mod focus_caps;
pub mod footer;
pub mod input_editor;
pub mod models_area;
pub(crate) mod models_area_view_model;
pub(crate) mod palette;
pub mod preflight;
pub(crate) mod render_view_model;
pub mod sheet;
pub mod status_line;
pub(crate) mod tree;
pub(crate) mod tree_view_model;
pub mod tui;

pub use crate::app as runtime;
pub use crate::dashboard;
pub use crate::picker;
pub use crate::smoke;
