//! Compatibility re-export. The model-universe assembly logic now lives in
//! `crate::logic::selection::assemble`; this shim is kept so callers using
//! the legacy `crate::selection::assemble::*` path keep compiling.

pub use crate::logic::selection::assemble::*;
