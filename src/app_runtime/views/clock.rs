//! Clock surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the system clock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClockView {
    /// Formatted wall-clock timestamp (e.g. "HH:MM:SS").
    pub timestamp: Arc<str>,
}
