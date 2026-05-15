//! Split pane surface view.
use serde::Serialize;

/// View projection for the split pane (horizontal dividing view).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SplitView {
    pub is_visible: bool,
    pub target: Option<SplitTargetView>,
}

/// Identifies what content the bottom split pane is displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SplitTargetView {
    /// An agent run transcript identified by its run ID.
    Run(u64),
    /// The Idea node's captured text or active input surface.
    Idea,
}
