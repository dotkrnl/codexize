//! Per-session aggregate view.
use super::chat::ChatView;
use super::clock::ClockView;
use super::config_panel::ConfigPanelView;
use super::footer::FooterView;
use super::modal::ModalKind;
use super::models::ModelsView;
use super::palette::PaletteView;
use super::picker::PickerView;
use super::render::RenderView;
use super::sheet::SheetView;
use super::split::SplitView;
use super::status_line::{StatusLineView, StatusMessage};
use super::tree::TreeView;
use super::watchdog::WatchdogView;
use crate::logic::pipeline::Stage;
use crate::state::RunStatus;
use serde::Serialize;
use std::sync::Arc;

/// Top-level view for a single session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionView {
    pub tree: TreeView,
    pub chat: ChatView,
    pub palette: PaletteView,
    pub status_line: StatusLineView,
    pub footer: FooterView,
    pub models: ModelsView,
    pub render: RenderView,
    pub config_panel: ConfigPanelView,
    pub picker: PickerView,
    pub sheet: SheetView,
    pub split: SplitView,
    pub clock: ClockView,
    pub watchdog: WatchdogView,
    pub modal: Option<ModalKind>,
    pub agent_runs: Arc<[AgentRunSummary]>,
    pub modes: ModeFlags,
    pub stage: Stage,
    pub status: Option<StatusMessage>,
}

/// Compact summary of an agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRunSummary {
    pub id: u64,
    pub stage: Arc<str>,
    pub window_name: Arc<str>,
    pub status: RunStatus,
}

impl AgentRunSummary {
    pub fn from_record(run: &crate::state::RunRecord) -> Self {
        Self {
            id: run.id,
            stage: Arc::from(run.stage.as_str()),
            window_name: Arc::from(run.window_name.as_str()),
            status: run.status,
        }
    }
}

/// Operator mode flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ModeFlags {
    pub yolo: bool,
    pub cheap: bool,
}
