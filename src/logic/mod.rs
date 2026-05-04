//! Canonical home for deterministic orchestration and policy logic.
//!
//! These re-exports are temporary compatibility seams for the layer split:
//! callers can start moving to `logic::*` paths before the physical modules
//! are relocated in later refactor slices.

pub use crate::artifacts;
pub use crate::final_validation as validation;
pub use crate::selection;
pub use crate::state;

pub mod pipeline {
    pub use crate::state::*;
}
