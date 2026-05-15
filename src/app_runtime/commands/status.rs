//! Status-line commands.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusCommand {
    /// Dismiss the active status-line message.
    Dismiss,
}
