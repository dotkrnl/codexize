//! Stage-targeted commands.
use crate::app_runtime::views::modal::StageId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageCommand {
    /// Retry the given stage from a non-modal palette path.
    Retry(StageId),
    /// Approve a pending review (spec or plan).
    Approve,
    /// Reject a pending review, requesting revisions.
    Reject,
    /// Manually start the current stage's agent.
    Start,
    /// Rewind to the previous stage if available.
    GoBack,
}
