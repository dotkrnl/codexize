use crate::logic::pipeline::state::PipelineItem;
use serde::Serialize;

#[path = "builder_legacy.rs"]
mod builder_legacy;
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
