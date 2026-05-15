//! Top-level operator-intent command for the runtime seam.
//!
//! `AppCommand` is the only way a UI expresses operator intent. Variants
//! are grouped by surface scope: truly global actions, project-shell-level
//! actions (sidebar, focus, lifecycle), and per-session actions addressed
//! by [`SessionId`]. Per-surface sub-command enums live under
//! [`crate::app_runtime::commands`].
//!
//! Terminal-specific input event types intentionally do NOT appear here:
//! each frontend translates its native input into one of the typed variants
//! under `commands/` before the value crosses the seam.
pub use super::commands::AppCommand;
