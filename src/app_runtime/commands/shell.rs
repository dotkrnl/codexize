//! Shell-level commands (cross-session: sidebar, focus, lifecycle).
use crate::app_runtime::root_view::SessionId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellCommand {
    /// Toggle the project sessions sidebar.
    ToggleSidebar,
    /// Focus the sidebar (when visible).
    FocusSidebar,
    /// Focus the workspace pane.
    FocusWorkspace,
    /// Toggle which of sidebar/workspace owns focus (Left/Right keys).
    ToggleSidebarFocus,
    /// Move the sidebar selection by `delta` rows (negative = up).
    MoveSidebarSelection { delta: isize },
    /// Open the session currently selected in the sidebar.
    OpenSelectedSidebarSession,
    /// Close the sidebar entirely (Esc when sidebar-focused, no modal).
    CloseSidebar,
    /// Switch focus to `session_id`, opening it if not already loaded.
    Focus(SessionId),
}
