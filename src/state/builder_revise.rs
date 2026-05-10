use super::BuilderState;
use crate::state::{PipelineItem, PipelineItemStatus};
impl BuilderState {
    /// Return the highest task ID ever seen across pipeline items, task titles,
    /// and recovery snapshots. Used to generate collision-free IDs when
    /// inserting new tasks from a revise verdict.
    pub fn max_task_id(&self) -> u32 {
        let from_pipeline = self
            .pipeline_items
            .iter()
            .filter_map(|i| i.task_id)
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
        from_pipeline.max(from_titles).max(from_recovery)
    }
    /// Handle a `revise` verdict that carries `new_tasks`: mark the current
    /// task as done in the pipeline, insert replacement tasks immediately after
    /// it, and renumber all later pending tasks to keep IDs monotonically
    /// increasing from the global maximum.
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
                    iteration: 1,
                },
            );
        }
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
        for (i, (title, _, _, _)) in new_tasks.iter().enumerate() {
            self.task_titles.insert(assigned_ids[i], title.clone());
        }
        for (old_id, new_id) in &renumber_map {
            if let Some(title) = self.task_titles.remove(old_id) {
                self.task_titles.insert(*new_id, title);
            }
        }
        self.last_verdict = Some("revise".to_string());
        assigned_ids
    }
}
