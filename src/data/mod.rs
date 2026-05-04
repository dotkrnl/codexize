//! Canonical home for persistence, process supervision, and provider IO.
//!
//! The current crate keeps existing module paths live while later slices move
//! files under this layer. Re-exports here are intentionally compatibility-only.

pub mod persistence;

pub use crate::acp;
pub use crate::adapters;
pub use crate::cache;
pub use crate::cache_lock;
pub use crate::providers;
pub use crate::runner;
pub use crate::synthetic_artifacts as synthetic;
pub use crate::warmup;
