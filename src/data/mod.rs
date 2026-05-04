//! Canonical home for persistence, process supervision, and provider IO.
//!
//! The current crate keeps existing module paths live while later slices move
//! files under this layer. Re-exports here are intentionally compatibility-only.

pub mod artifacts;
pub mod cache;
pub mod cache_lock;
pub mod persistence;
pub mod synthetic_artifacts;
pub mod warmup;

pub use crate::acp;
pub use crate::adapters;
pub use crate::providers;
pub use crate::runner;
pub use crate::data::synthetic_artifacts as synthetic;
