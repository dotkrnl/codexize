//! Crate root for the application binary and integration tests.
//!
//! The implementation is split into [`logic`] (pure orchestration), [`data`]
//! (IO + state custody), [`ui`] (terminal rendering), and [`app_runtime`]
//! (runtime-facing command/view adapters). Callers import through those owning
//! modules instead of root-level aliases.
pub mod app;
pub mod app_runtime;
pub mod app_shell;
pub mod coder_summary;
pub mod data;
pub mod diagnostics;
pub mod lifecycle;
pub mod logic;
pub mod model_names;
pub mod review;
pub mod scheduler;
pub mod selection;
pub mod simplification;
pub mod state;
pub mod tasks;
pub mod ui;
