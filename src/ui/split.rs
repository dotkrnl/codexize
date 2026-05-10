/// Identifies what content the bottom split pane is displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitTarget {
    /// An agent run transcript identified by its run id.
    Run(u64),
    /// The Idea node's captured text or active input surface.
    Idea,
}
pub(crate) fn run_main_panel_message_visible(
    _run: &crate::state::RunRecord,
    kind: crate::state::MessageKind,
    _show_thinking_texts: bool,
) -> bool {
    use crate::state::MessageKind;
    matches!(
        kind,
        MessageKind::Started
            | MessageKind::Brief
            | MessageKind::Summary
            | MessageKind::SummaryWarn
            | MessageKind::End
    )
}
pub(crate) fn run_split_panel_message_visible(
    _run: &crate::state::RunRecord,
    kind: crate::state::MessageKind,
    show_thinking_texts: bool,
) -> bool {
    use crate::state::MessageKind;
    match kind {
        MessageKind::AgentText => true,
        // User input is the approved exception to the split's ACP/debug focus.
        MessageKind::UserInput => true,
        MessageKind::AgentThought => show_thinking_texts,
        MessageKind::Started | MessageKind::End => true,
        MessageKind::Brief | MessageKind::Summary | MessageKind::SummaryWarn => false,
    }
}
#[cfg(test)]
#[path = "split_tests.rs"]
mod tests;
