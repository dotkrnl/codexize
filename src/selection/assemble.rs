//! Compatibility re-export. The IO-orchestrating wrappers
//! (`assemble_models`, `assemble_from_cached_only`, `assemble_from_loaded`)
//! live in the data layer (`crate::data::selection_assembly`); the pure
//! merge/collapse/ranking helpers live in `crate::logic::selection::assemble`.
//! This shim is kept so callers using the legacy `crate::selection::assemble::*`
//! path keep compiling.

pub use crate::data::selection_assembly::{
    assemble_from_cached_only, assemble_from_loaded, assemble_models,
};
pub use crate::logic::selection::assemble::*;
