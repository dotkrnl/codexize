//! Tree navigation commands.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TreeCommand {
    /// Scroll viewport by one row in `delta`'s direction, or move focus
    /// if the viewport is already pinned at the edge.
    ScrollOrMoveFocus { delta: isize },
    /// Move focus by `delta` rows without scrolling first.
    MoveFocus { delta: isize },
    /// Page up/down by viewport height.
    ScrollViewportPage { delta: isize },
    /// Toggle expand/collapse on the focused tree node.
    ToggleExpand,
    /// Activate the focused row: open the split pane on its target if any,
    /// otherwise retry/start the selected retry target.
    ActivateFocused,
}
