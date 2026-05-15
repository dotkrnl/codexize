//! Tree surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the navigation tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TreeView {
    pub rows: Arc<[VisibleNodeRow]>,
    pub selected_index: Option<usize>,
}

/// One visible row in the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibleNodeRow {
    pub depth: usize,
    pub label: Arc<str>,
    pub status: TreeNodeStatus,
    pub has_children: bool,
    pub is_expanded: bool,
    /// For recovery/runs: the underlying run ID if any.
    pub run_id: Option<u64>,
}

/// UI-neutral status for a tree node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TreeNodeStatus {
    Pending,
    Running,
    Success,
    Failure,
    Skipped,
}
