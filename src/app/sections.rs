use crate::state::SessionState;
use super::state::{PipelineSection, SectionStatus};

pub(super) fn build_sections(_state: &SessionState, _window_launched: bool) -> Vec<PipelineSection> {
    // TODO: rebuild with tree (Task 2)
    Vec::new()
}

pub(super) fn current_section_index(_sections: &[PipelineSection]) -> usize {
    0
}