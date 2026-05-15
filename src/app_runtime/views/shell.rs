//! Shell-level (cross-session) state.
use crate::logic::pipeline::Stage;
use serde::Serialize;

/// Shell-level view mirroring today's sidebar and focus state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ShellView {
    pub visible: bool,
    pub focus: ShellFocus,
    pub selected_index: usize,
    pub rows: Vec<SidebarRow>,
}

/// One row in the project sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SidebarRow {
    pub session_id: String,
    pub date_label: String,
    pub title: String,
    pub stage: Stage,
    pub focused: bool,
    pub open: bool,
    pub running: bool,
}

/// Top-level shell focus area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ShellFocus {
    #[default]
    Workspace,
    Sidebar,
}
