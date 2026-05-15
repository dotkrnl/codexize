//! Split-pane commands.
use crate::app_runtime::views::split::SplitTargetView;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitCommand {
    Open(SplitTargetView),
    /// Open the split on whatever the focused tree row resolves to.
    OpenFocused,
    Close,
    ScrollLines { delta: isize },
    ScrollPages { delta: isize },
}
