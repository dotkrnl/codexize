//! Shell-level (cross-session) state.
use crate::logic::pipeline::Stage;
use serde::Serialize;
use std::sync::Arc;

/// Shell-level view mirroring today's sidebar and focus state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ShellView {
    pub sidebar_visible: bool,
    pub focus: ShellFocus,
    pub selected_index: usize,
    pub rows: Arc<[SidebarRow]>,
}

/// One row in the project sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SidebarRow {
    pub session_id: Arc<str>,
    pub date_label: Arc<str>,
    pub title: Arc<str>,
    pub stage: Stage,
    pub focused: bool,
    pub open: bool,
    pub running: bool,
}

/// Top-level shell focus area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ShellFocus {
    Workspace,
    Sidebar,
}

impl Default for ShellFocus {
    fn default() -> Self {
        Self::Workspace
    }
}
