use super::{PipelineItem, PipelineItemStatus};
use serde::{Deserialize, Serialize};

/// Tracks the builder loop — which tasks are pending, done, what iteration
/// we're on, and enough state to resume a killed session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuilderState {
    #[serde(default)]
    pub pipeline_items: Vec<PipelineItem>,
    // Legacy fields — callers migrate to pipeline_items in Phase 3.
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
}

impl PipelineItemStatus {
    pub fn is_lifecycle(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Running | Self::Done | Self::Failed
        )
    }

    pub fn is_verdict(self) -> bool {
        matches!(
            self,
            Self::Approved | Self::Revise | Self::HumanBlocked | Self::AgentPivot
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done
                | Self::Failed
                | Self::Approved
                | Self::Revise
                | Self::HumanBlocked
                | Self::AgentPivot
        )
    }
}

impl BuilderState {
    fn is_selectable_task_item(item: &PipelineItem) -> bool {
        matches!(item.status, PipelineItemStatus::Pending)
            || (item.status == PipelineItemStatus::Revise
                && item.mode.as_deref() != Some("superseded"))
    }

    fn pipeline_task_items(&self) -> impl Iterator<Item = &PipelineItem> {
        self.pipeline_items
            .iter()
            .filter(|item| item.stage == "coder" && item.task_id.is_some())
    }

    pub fn next_pipeline_id(&self) -> u32 {
        self.pipeline_items.iter().map(|i| i.id).max().unwrap_or(0) + 1
    }

    pub fn push_pipeline_item(&mut self, mut item: PipelineItem) -> u32 {
        if item.id == 0 {
            item.id = self.next_pipeline_id();
        }
        let id = item.id;
        self.pipeline_items.push(item);
        id
    }

    pub fn get_pipeline_item(&self, id: u32) -> Option<&PipelineItem> {
        self.pipeline_items.iter().find(|i| i.id == id)
    }

    pub fn get_pipeline_item_mut(&mut self, id: u32) -> Option<&mut PipelineItem> {
        self.pipeline_items.iter_mut().find(|i| i.id == id)
    }

    pub fn update_pipeline_status(&mut self, id: u32, status: PipelineItemStatus) -> bool {
        if let Some(item) = self.get_pipeline_item_mut(id) {
            item.status = status;
            true
        } else {
            false
        }
    }

    pub fn pipeline_items_by_stage(&self, stage: &str) -> Vec<&PipelineItem> {
        self.pipeline_items
            .iter()
            .filter(|i| i.stage == stage)
            .collect()
    }

    pub fn pending_pipeline_items(&self) -> Vec<&PipelineItem> {
        self.pipeline_items
            .iter()
            .filter(|i| i.status == PipelineItemStatus::Pending)
            .collect()
    }

    pub fn running_pipeline_items(&self) -> Vec<&PipelineItem> {
        self.pipeline_items
            .iter()
            .filter(|i| i.status == PipelineItemStatus::Running)
            .collect()
    }

    pub fn reset_task_pipeline(&mut self, tasks: impl IntoIterator<Item = (u32, Option<String>)>) {
        self.pipeline_items.clear();
        for (task_id, title) in tasks {
            self.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "coder".to_string(),
                task_id: Some(task_id),
                round: None,
                status: PipelineItemStatus::Pending,
                title,
                mode: None,
                trigger: None,
                interactive: None,
            });
        }
        self.iteration = 0;
        self.last_verdict = None;
        self.sync_legacy_queue_views();
    }

    pub fn current_task_id(&self) -> Option<u32> {
        if self.pipeline_items.is_empty() {
            return self.current_task;
        }
        self.pipeline_task_items()
            .find(|item| item.status == PipelineItemStatus::Running)
            .and_then(|item| item.task_id)
    }

    pub fn done_task_ids(&self) -> Vec<u32> {
        if self.pipeline_items.is_empty() {
            return self.done.clone();
        }
        self.pipeline_task_items()
            .filter(|item| {
                matches!(
                    item.status,
                    PipelineItemStatus::Approved | PipelineItemStatus::Done
                )
            })
            .filter_map(|item| item.task_id)
            .collect()
    }

    pub fn pending_task_ids(&self) -> Vec<u32> {
        if self.pipeline_items.is_empty() {
            return self.pending.clone();
        }
        self.pipeline_task_items()
            .filter(|item| Self::is_selectable_task_item(item))
            .filter_map(|item| item.task_id)
            .collect()
    }

    pub fn has_unfinished_tasks(&self) -> bool {
        if self.pipeline_items.is_empty() {
            return self.current_task.is_some() || !self.pending.is_empty();
        }
        self.pipeline_task_items().any(|item| {
            item.status == PipelineItemStatus::Running
                || item.status == PipelineItemStatus::HumanBlocked
                || item.status == PipelineItemStatus::AgentPivot
                || Self::is_selectable_task_item(item)
        })
    }

    pub fn ensure_task_for_round(&mut self, round: u32) -> Option<u32> {
        if self.pipeline_items.is_empty() {
            // REVIEWER: legacy queue fallback is kept so older tests that seed only
            // pending/current_task continue to run; runtime sharding/skip/recovery
            // initialization now always populates pipeline_items first.
            if self.current_task.is_none() {
                if let Some(id) = self.pending.first().copied() {
                    self.pending.remove(0);
                    self.current_task = Some(id);
                } else {
                    return None;
                }
            }
            self.iteration = round;
            return self.current_task;
        }

        if let Some(index) = self.pipeline_items.iter().position(|item| {
            item.stage == "coder"
                && item.task_id.is_some()
                && item.status == PipelineItemStatus::Running
        }) {
            self.pipeline_items[index].round = Some(round);
            self.iteration = round;
            self.sync_legacy_queue_views();
            return self.pipeline_items[index].task_id;
        }

        if let Some(index) = self.pipeline_items.iter().position(|item| {
            item.stage == "coder" && item.task_id.is_some() && Self::is_selectable_task_item(item)
        }) {
            self.pipeline_items[index].status = PipelineItemStatus::Running;
            self.pipeline_items[index].round = Some(round);
            self.iteration = round;
            let task_id = self.pipeline_items[index].task_id;
            self.sync_legacy_queue_views();
            return task_id;
        }

        None
    }

    pub fn set_task_status(
        &mut self,
        task_id: u32,
        status: PipelineItemStatus,
        round: Option<u32>,
    ) -> bool {
        if self.pipeline_items.is_empty() {
            match status {
                PipelineItemStatus::Pending => {
                    if self.current_task == Some(task_id) {
                        self.current_task = None;
                    }
                    true
                }
                PipelineItemStatus::Approved => {
                    if self.current_task == Some(task_id) {
                        self.current_task = None;
                    }
                    if !self.done.contains(&task_id) {
                        self.done.push(task_id);
                    }
                    self.pending.retain(|id| *id != task_id);
                    self.last_verdict = Some("approved".to_string());
                    true
                }
                PipelineItemStatus::Revise => {
                    self.current_task = Some(task_id);
                    self.last_verdict = Some("revise".to_string());
                    true
                }
                PipelineItemStatus::HumanBlocked => {
                    self.last_verdict = Some("human_blocked".to_string());
                    true
                }
                PipelineItemStatus::AgentPivot => {
                    self.last_verdict = Some("agent_pivot".to_string());
                    true
                }
                _ => false,
            }
        } else if let Some(index) = self
            .pipeline_items
            .iter()
            .position(|item| item.stage == "coder" && item.task_id == Some(task_id))
        {
            self.pipeline_items[index].status = status;
            if round.is_some() {
                self.pipeline_items[index].round = round;
            }
            self.sync_legacy_queue_views();
            true
        } else {
            false
        }
    }

    pub fn sync_legacy_queue_views(&mut self) {
        if self.pipeline_items.is_empty() {
            return;
        }
        self.done = self.done_task_ids();
        self.current_task = self.current_task_id();
        self.pending = self.pending_task_ids();
    }

    /// Return the highest task ID ever seen across pipeline items, legacy
    /// queues, task_titles, and recovery snapshots. Used to generate
    /// collision-free IDs when inserting new tasks from a revise verdict.
    pub fn max_task_id(&self) -> u32 {
        let from_pipeline = self
            .pipeline_items
            .iter()
            .filter_map(|i| i.task_id)
            .max()
            .unwrap_or(0);
        let from_legacy = self
            .done
            .iter()
            .chain(self.pending.iter())
            .chain(self.current_task.iter())
            .copied()
            .max()
            .unwrap_or(0);
        let from_titles = self.task_titles.keys().copied().max().unwrap_or(0);
        let from_recovery = self
            .recovery_prev_task_ids
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .max(self.recovery_prev_max_task_id.unwrap_or(0));
        from_pipeline
            .max(from_legacy)
            .max(from_titles)
            .max(from_recovery)
    }

    /// Handle a `revise` verdict that carries `new_tasks`: mark the current
    /// task as done in the pipeline, insert replacement tasks immediately after
    /// it, and renumber all later pending tasks to keep IDs monotonically
    /// increasing from the global maximum.
    ///
    /// Each entry in `new_tasks` is `(title, description, test, estimated_tokens)`.
    /// Returns the list of newly assigned task IDs.
    pub fn apply_revise_with_new_tasks(
        &mut self,
        current_task_id: u32,
        new_tasks: Vec<(String, String, String, u32)>,
    ) -> Vec<u32> {
        if new_tasks.is_empty() {
            return vec![];
        }

        let current_idx = self
            .pipeline_items
            .iter()
            .position(|item| item.stage == "coder" && item.task_id == Some(current_task_id));

        let insert_pos = match current_idx {
            Some(idx) => {
                self.pipeline_items[idx].status = PipelineItemStatus::Revise;
                self.pipeline_items[idx].mode = Some("superseded".to_string());
                idx + 1
            }
            None => self.pipeline_items.len(),
        };

        let mut next_id = self.max_task_id() + 1;
        let mut assigned_ids = Vec::with_capacity(new_tasks.len());

        for (title, _desc, _test, _tokens) in &new_tasks {
            let task_id = next_id;
            next_id += 1;
            assigned_ids.push(task_id);
            let pipeline_id = self.next_pipeline_id();
            self.pipeline_items.insert(
                insert_pos + assigned_ids.len() - 1,
                PipelineItem {
                    id: pipeline_id,
                    stage: "coder".to_string(),
                    task_id: Some(task_id),
                    round: None,
                    status: PipelineItemStatus::Pending,
                    title: Some(title.clone()),
                    mode: None,
                    trigger: None,
                    interactive: None,
                },
            );
        }

        // Renumber later pending coder items that follow the inserted block.
        // Their old task IDs become the new monotonic continuation.
        let renumber_start = insert_pos + new_tasks.len();
        let mut renumber_map = std::collections::BTreeMap::new();
        for item in &mut self.pipeline_items[renumber_start..] {
            if item.stage == "coder"
                && item.status == PipelineItemStatus::Pending
                && let Some(old_id) = item.task_id
            {
                let new_id = next_id;
                next_id += 1;
                renumber_map.insert(old_id, new_id);
                item.task_id = Some(new_id);
            }
        }

        // Update task_titles for new and renumbered tasks.
        for (i, (title, _, _, _)) in new_tasks.iter().enumerate() {
            self.task_titles.insert(assigned_ids[i], title.clone());
        }
        for (old_id, new_id) in &renumber_map {
            if let Some(title) = self.task_titles.remove(old_id) {
                self.task_titles.insert(*new_id, title);
            }
        }

        self.last_verdict = Some("revise".to_string());
        self.sync_legacy_queue_views();
        assigned_ids
    }
}
