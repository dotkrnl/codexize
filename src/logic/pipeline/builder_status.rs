use crate::logic::pipeline::state::PipelineItemStatus;

impl PipelineItemStatus {
    pub fn is_lifecycle(self) -> bool {
        self.is_pending() || self.is_running() || self.is_done() || self.is_failed()
    }

    pub fn is_verdict(self) -> bool {
        self.is_approved() || self.is_revise() || self.is_human_blocked() || self.is_agent_pivot()
    }

    pub fn is_terminal(self) -> bool {
        self.is_done() || self.is_failed() || self.is_verdict()
    }
}
