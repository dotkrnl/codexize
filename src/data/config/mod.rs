//! Unified operator config (`~/.codexize/config.toml`).
//!
//! Owns the schema, loader, baked defaults, paths, and typed views for the
//! single global config file that replaces `ntfy.toml` and the scattered
//! `const`/`Default` knobs across the codebase.

pub mod cli;
pub mod defaults;
pub mod fmt;
pub mod loader;
pub mod mutate;
pub mod paths;
pub mod schema;
pub mod view;

pub use loader::{LoadError, load_or_default, save_atomic, save_atomic_to};
pub use paths::config_path;
pub use schema::{Config, Override};
pub use view::{NtfyEventsView, NtfyView};
