use crate::state::{PipelineItem, PipelineItemStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[path = "builder_queue.rs"]
mod builder_queue;
#[path = "builder_revise.rs"]
mod builder_revise;
#[path = "builder_status.rs"]
mod builder_status;
#[cfg(test)]
#[path = "builder_tests.rs"]
mod builder_tests;

/// Tracks the builder loop — which tasks are pending, done, what iteration
/// we're on, and enough state to resume a killed session.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BuilderState {
    #[serde(default)]
    pub pipeline_items: Vec<PipelineItem>,
    // Compatibility views for older session files; mutators derive them from pipeline_items.
    #[serde(default)]
    pub pending: Vec<u32>,
    #[serde(default)]
    pub done: Vec<u32>,
    #[serde(default)]
    pub current_task: Option<u32>,
    /// Global iteration counter — one coder+reviewer cycle is one iteration.
    #[serde(default)]
    pub iteration: u32,
    #[serde(default)]
    pub last_verdict: Option<String>,
    /// Refine feedback carried forward from a `refine` reviewer verdict on
    /// the previous task. Drained into the next coder prompt and cleared.
    #[serde(default)]
    pub pending_refine_feedback: Vec<String>,
    /// Recovery context captured when entering builder recovery.
    ///
    /// Orchestrator-owned: the recovery agent may edit artifacts, but it must not
    /// mutate queue state directly; reconciliation uses this context plus run
    /// history to enforce invariants.
    #[serde(default)]
    pub recovery_trigger_task_id: Option<u32>,
    /// Maximum task id observed before recovery began (from the pre-recovery tasks.toml).
    #[serde(default)]
    pub recovery_prev_max_task_id: Option<u32>,
    /// Full task id set observed before recovery began.
    #[serde(default)]
    pub recovery_prev_task_ids: Vec<u32>,
    /// Optional human-readable trigger summary (e.g. retry exhaustion details).
    #[serde(default)]
    pub recovery_trigger_summary: Option<String>,
    /// Builder retry reset boundary: failed coder/reviewer runs at or before this
    /// run id are ignored when rebuilding retry exclusions after restart.
    #[serde(default)]
    pub retry_reset_run_id_cutoff: Option<u64>,
    /// How many recovery cycles have been entered since the last successful
    /// recovery. The circuit-breaker escalates to `human_blocked` when this
    /// reaches 3, preventing infinite recovery loops.
    #[serde(default)]
    pub recovery_cycle_count: u32,
    /// Short one-line titles keyed by task id, sourced from tasks.toml.
    /// Used to label task nodes in the pipeline tree.
    #[serde(default)]
    pub task_titles: std::collections::BTreeMap<u32, String>,
    /// One-shot iteration override consumed by `recovery_outer_iteration` when
    /// the operator triggers recovery from a `BlockedNeedsUser + FinalValidation`
    /// modal. `None` for the reviewer-driven `human_blocked` path so its
    /// existing iteration semantics stay intact.
    #[serde(default)]
    pub next_iteration_for_recovery: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct BuilderStateWire {
    #[serde(default)]
    pipeline_items: Vec<PipelineItem>,
    #[serde(default)]
    pending: Vec<u32>,
    #[serde(default)]
    done: Vec<u32>,
    #[serde(default)]
    current_task: Option<u32>,
    #[serde(default)]
    iteration: u32,
    #[serde(default)]
    last_verdict: Option<String>,
    #[serde(default)]
    pending_refine_feedback: Vec<String>,
    #[serde(default)]
    recovery_trigger_task_id: Option<u32>,
    #[serde(default)]
    recovery_prev_max_task_id: Option<u32>,
    #[serde(default)]
    recovery_prev_task_ids: Vec<u32>,
    #[serde(default)]
    recovery_trigger_summary: Option<String>,
    #[serde(default)]
    retry_reset_run_id_cutoff: Option<u64>,
    #[serde(default)]
    recovery_cycle_count: u32,
    #[serde(default)]
    task_titles: std::collections::BTreeMap<u32, String>,
    #[serde(default)]
    next_iteration_for_recovery: Option<u32>,
}

impl<'de> Deserialize<'de> for BuilderState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = BuilderStateWire::deserialize(deserializer)?;
        let mut state = Self {
            pipeline_items: wire.pipeline_items,
            pending: wire.pending,
            done: wire.done,
            current_task: wire.current_task,
            iteration: wire.iteration,
            last_verdict: wire.last_verdict,
            pending_refine_feedback: wire.pending_refine_feedback,
            recovery_trigger_task_id: wire.recovery_trigger_task_id,
            recovery_prev_max_task_id: wire.recovery_prev_max_task_id,
            recovery_prev_task_ids: wire.recovery_prev_task_ids,
            recovery_trigger_summary: wire.recovery_trigger_summary,
            retry_reset_run_id_cutoff: wire.retry_reset_run_id_cutoff,
            recovery_cycle_count: wire.recovery_cycle_count,
            task_titles: wire.task_titles,
            next_iteration_for_recovery: wire.next_iteration_for_recovery,
        };
        state.hydrate_legacy_pipeline_items();
        Ok(state)
    }
}

impl BuilderState {
    fn hydrate_legacy_pipeline_items(&mut self) {
        if !self.pipeline_items.is_empty() {
            self.sync_legacy_queue_views();
            return;
        }
        if self.done.is_empty() && self.current_task.is_none() && self.pending.is_empty() {
            return;
        }

        let mut seen = BTreeSet::new();
        let mut next_pipeline_id = 1;
        for (task_id, status, round) in self
            .done
            .iter()
            .copied()
            .map(|task_id| (task_id, PipelineItemStatus::Approved, None))
            .chain(self.current_task.map(|task_id| {
                let round = (self.iteration > 0).then_some(self.iteration);
                (task_id, PipelineItemStatus::Running, round)
            }))
            .chain(
                self.pending
                    .iter()
                    .copied()
                    .map(|task_id| (task_id, PipelineItemStatus::Pending, None)),
            )
        {
            if !seen.insert(task_id) {
                continue;
            }
            // Legacy queues only persisted membership; historical completed
            // items map to Approved because reviewer verdicts drove `done`.
            self.pipeline_items.push(PipelineItem {
                id: next_pipeline_id,
                stage: "coder".to_string(),
                task_id: Some(task_id),
                round,
                status,
                title: self.task_titles.get(&task_id).cloned(),
                mode: None,
                trigger: None,
                interactive: None,
                iteration: 1,
            });
            next_pipeline_id += 1;
        }
        self.sync_legacy_queue_views();
    }
}
