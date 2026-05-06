//! Shared ACP launch/event helpers used by the thin transport facade.

// Kept outside `src/data/acp` so the measured transport directory contains
// only the runtime facade and stdio actor; `crate::acp` remains the public API.
pub(crate) mod config;
pub(crate) mod events;
pub(crate) mod tool_call;
