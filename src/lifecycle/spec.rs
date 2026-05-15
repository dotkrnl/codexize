//! Stage launch spec and active run record.
//!
//! [`StageSpec`] is the descriptor the FSM hands to a launcher — "this is
//! what the next agent run should be". [`ActiveRun`] is what the launcher
//! hands back once the run is confirmed running.
//!
//! The spec stays intentionally small: launch-side details such as model,
//! effort, modes, and prompt path are still resolved by the existing launcher
//! path.
use super::stage_id::StageId;
use crate::state::RunRecord;
use serde::{Deserialize, Serialize};

/// Descriptor for a stage attempt the lifecycle wants to launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSpec {
    pub stage_id: StageId,
    pub round: u32,
    pub task_id: Option<u32>,
    pub attempt: u32,
    pub window_name: String,
}

impl StageSpec {
    pub fn from_run_record(run: &RunRecord) -> Option<Self> {
        Some(Self {
            stage_id: super::stage_id::stage_id_for_run(&run.stage, &run.window_name)?,
            round: run.round,
            task_id: run.task_id,
            attempt: run.attempt,
            window_name: run.window_name.clone(),
        })
    }

    /// Return a copy of this spec with `attempt` incremented by one.
    ///
    /// Used by [`super::fsm::AfterStop::Restart`] to derive the next attempt's
    /// spec from the current one. Every other field — including
    /// `window_name` — is preserved verbatim; callers that need a fresh window
    /// name should rebuild via [`super::stage::Stage::build_spec`] instead.
    pub fn with_attempt_plus_one(self) -> Self {
        Self {
            attempt: self.attempt.saturating_add(1),
            ..self
        }
    }
}

/// A run the FSM has been told is live. Created on
/// [`super::fsm::Fsm::confirm_running`]; carried through to
/// [`super::fsm::FinalizedRun`] when the run terminates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRun {
    pub run_id: u64,
    pub spec: StageSpec,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl ActiveRun {
    pub fn from_run_record(run: &RunRecord) -> Option<Self> {
        Some(Self {
            run_id: run.id,
            spec: StageSpec::from_run_record(run)?,
            started_at: run.started_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> StageSpec {
        StageSpec {
            stage_id: StageId::Brainstorm,
            round: 1,
            task_id: None,
            attempt: 1,
            window_name: "brainstorm-1".to_string(),
        }
    }

    #[test]
    fn with_attempt_plus_one_increments_only_attempt() {
        let original = sample_spec();
        let next = original.clone().with_attempt_plus_one();
        assert_eq!(next.attempt, original.attempt + 1);
        assert_eq!(next.stage_id, original.stage_id);
        assert_eq!(next.round, original.round);
        assert_eq!(next.task_id, original.task_id);
        assert_eq!(next.window_name, original.window_name);
    }

    #[test]
    fn with_attempt_plus_one_saturates_on_overflow() {
        let spec = StageSpec {
            attempt: u32::MAX,
            ..sample_spec()
        };
        assert_eq!(spec.with_attempt_plus_one().attempt, u32::MAX);
    }
}
