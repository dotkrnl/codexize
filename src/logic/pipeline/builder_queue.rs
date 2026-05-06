use super::BuilderState;
use crate::logic::pipeline::state::{PipelineItem, PipelineItemStatus};

impl BuilderState {
    pub(super) fn is_selectable_task_item(item: &PipelineItem) -> bool {
        matches!(item.status, PipelineItemStatus::Pending)
            || (item.status == PipelineItemStatus::Revise
                && item.mode.as_deref() != Some("superseded"))
    }

    pub(super) fn pipeline_task_items(&self) -> impl Iterator<Item = &PipelineItem> {
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
                iteration: 1,
            });
        }
        self.iteration = 0;
        self.last_verdict = None;
        self.sync_legacy_queue_views();
    }

    pub fn current_task_id(&self) -> Option<u32> {
        self.pipeline_task_items()
            .find(|item| item.status == PipelineItemStatus::Running)
            .and_then(|item| item.task_id)
    }

    pub fn done_task_ids(&self) -> Vec<u32> {
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
        self.pipeline_task_items()
            .filter(|item| Self::is_selectable_task_item(item))
            .filter_map(|item| item.task_id)
            .collect()
    }

    pub fn has_unfinished_tasks(&self) -> bool {
        self.pipeline_task_items().any(|item| {
            item.status == PipelineItemStatus::Running
                || item.status == PipelineItemStatus::HumanBlocked
                || item.status == PipelineItemStatus::AgentPivot
                || Self::is_selectable_task_item(item)
        })
    }

    pub fn ensure_task_for_round(&mut self, round: u32) -> Option<u32> {
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
        if let Some(index) = self
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
}
