//! Unified operator config (`~/.codexize/config.toml`).
//!
//! Owns the schema, loader, baked defaults, paths, and typed views for the
//! single global config file that replaces `ntfy.toml` and the scattered
//! `const`/`Default` knobs across the codebase. Subsystem rewiring (the
//! actual readers of these views) lands in later milestones; this module
//! is the foundation everything else consumes.

pub mod defaults;
pub mod loader;
pub mod paths;
pub mod schema;
pub mod view;

pub use loader::{LoadError, save_atomic};
pub use paths::config_path;
pub use schema::{Config, Override};
