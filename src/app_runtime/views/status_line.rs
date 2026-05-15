//! Status line surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the transient status line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StatusLineView {
    pub current: Option<StatusMessage>,
}

/// Single line of operator-facing status text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusMessage {
    pub text: Arc<str>,
    pub severity: StatusSeverity,
}

/// Severity tag for a status message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum StatusSeverity {
    Info,
    Warn,
    Error,
}
