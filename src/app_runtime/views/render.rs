//! Render-level hints and global animation state.
use serde::Serialize;

/// View projection for global rendering hints (spinners, cache invalidation).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RenderView {
    /// Monotonic tick for global animations (spinners).
    pub spinner_tick: usize,
}

pub(crate) use crate::app::render_helpers::{strip_ansi, wrap_text};
