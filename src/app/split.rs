/// Identifies what content the bottom split pane is displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitTarget {
    /// An agent run transcript identified by its run id.
    Run(u64),
    /// The Idea node's captured text or active input surface.
    Idea,
}

pub(super) fn run_split_message_visible(
    run: &crate::state::RunRecord,
    kind: crate::state::MessageKind,
    show_noninteractive_texts: bool,
    show_thinking_texts: bool,
) -> bool {
    if run.modes.interactive {
        return match kind {
            crate::state::MessageKind::AgentText => true,
            crate::state::MessageKind::AgentThought => show_thinking_texts,
            _ => false,
        };
    }

    kind.visible_with_filters(show_noninteractive_texts, show_thinking_texts)
}
