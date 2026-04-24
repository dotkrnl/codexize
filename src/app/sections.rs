use crate::state::SessionState;

use super::state::PipelineSection;

pub(super) fn build_sections(_state: &SessionState, _window_launched: bool) -> Vec<PipelineSection> {
    vec![PipelineSection::action(
        "Pipeline View",
        "TODO(Task 2): rebuild the pipeline tree against schema v2",
        vec![
            "State schema v2 and message persistence are implemented.",
            "App rendering remains a temporary placeholder until Task 2 lands.",
        ],
    )]
}

pub(super) fn current_section_index(_sections: &[PipelineSection]) -> usize {
    0
}
