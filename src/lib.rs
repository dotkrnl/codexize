//! Crate root.
//!
//! The implementation is split into three layered trees: [`logic`] (pure
//! orchestration), [`data`] (IO + state custody), and [`ui`] (terminal
//! rendering). The `pub use` aliases below flatten frequently-used
//! sub-modules into a single `crate::*` namespace consumed by `main.rs`,
//! the integration tests, and the future server-mode binary.
pub use crate::data::acp;
pub use crate::data::adapters;
pub mod app;
pub mod app_runtime;
pub mod app_shell;
pub use crate::data::artifacts;
pub use crate::data::cache;
pub use crate::data::cache_lock;
pub mod coder_summary;
pub use crate::data::dashboard_io as dashboard;
pub mod data;
pub mod diagnostics;
pub use crate::data::validation as final_validation;
pub use crate::ui::input_editor;
pub mod logic;
pub mod model_names;
pub use crate::data::providers;
pub use crate::ui::preflight;
pub use crate::ui::widgets::picker::state as picker;
pub mod review;
pub use crate::data::runner;
pub mod scheduler;
pub mod selection;
pub mod simplification;
pub use crate::data::snapshot as smoke;
pub mod state;
pub use crate::data::synthetic_artifacts;
pub mod tasks;
pub use crate::ui::tui;
pub mod ui;
pub use crate::data::warmup;
