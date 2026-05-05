// Re-organized from tests_finalization.rs — see commit history.

use super::*;
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    selection,
    state::{
        self as session_state, MessageKind, Phase, PipelineItem, PipelineItemStatus, RunRecord,
        RunStatus, SessionState,
    },
    tasks,
};

pub(super) fn make_simplifier_run(id: u64, round: u32, attempt: u32) -> RunRecord {
    RunRecord {
        id,
        stage: "simplifier".to_string(),
        task_id: None,
        round,
        attempt,
        model: "claude-sonnet-4-6".to_string(),
        vendor: "claude".to_string(),
        window_name: "[Simplifier]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

mod chunk_00_tests;
mod chunk_01_tests;
mod chunk_02_tests;
mod chunk_03_tests;
mod chunk_04_tests;
