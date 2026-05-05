//! Canonical home for persistence, process supervision, and provider IO.
//!
//! The current crate keeps existing module paths live while later slices move
//! files under this layer. Re-exports here are intentionally compatibility-only.

pub mod acp;
pub mod adapters;
pub mod artifacts;
pub mod cache;
pub mod cache_lock;
pub mod dashboard_io;
pub(crate) mod dashboard_model;
pub mod events;
pub mod observation;
pub mod persistence;
pub mod picker_io;
pub mod preflight;
pub mod providers;
pub mod runner;
pub mod selection_assembly;
pub mod snapshot;
pub mod synthetic_artifacts;
pub mod validation;
pub mod warmup;
pub use crate::data::events::{DataEvent, DataOutcome, DataRequest, dispatch};
pub use crate::data::synthetic_artifacts as synthetic;
