//! V2 persistence shapes for the lifecycle redesign.
//!
//! Added alongside the existing `SessionState`/`RunRecord` in
//! [`crate::state::types`] — neither replaces nor coexists with them at
//! runtime in Step 1. The cutover (Step 5) swaps the on-disk format outright;
//! there is no migration path and no dual-read.
use super::fsm::Outcome;
use super::pending::PendingDecisions;
use super::phase::Phase;
use crate::adapters::EffortLevel;
use crate::app_runtime::view::StageId;
use crate::data::config::schema::EffortMapping;
use crate::state::{LaunchModes, Modes};
use serde::{Deserialize, Serialize};

/// V2 on-disk shape of a session file. Mirrors the slim
/// [`super::phase::Phase`] and the [`PendingDecisions`] aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFileV2 {
    pub session_id: String,
    pub idea_text: Option<String>,
    pub phase: Phase,
    pub modes: Modes,
    pub paused_at_phase: Option<Phase>,
    pub pending_decisions: PendingDecisions,
}

/// V2 on-disk shape of a single run record.
///
/// `outcome = None` corresponds to the legacy `status = Running` row — a run
/// the TUI knew was live but had not yet seen finalize. Resume backfills
/// these with [`Outcome::Aborted`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecordV2 {
    pub id: u64,
    pub stage_id: StageId,
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
    pub window_name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub outcome: Option<Outcome>,
    pub model: String,
    pub subscription_label: String,
    pub effort: EffortLevel,
    pub effort_mapping: EffortMapping,
    pub effort_eligible: bool,
    pub modes: LaunchModes,
    pub hostname: Option<String>,
    pub mount_device_id: Option<u64>,
}
