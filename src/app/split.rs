use crate::state::{MessageKind, RunRecord};

pub(crate) fn run_main_panel_message_visible(
    _run: &RunRecord,
    kind: MessageKind,
    _show_thinking_texts: bool,
) -> bool {
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
    _run: &RunRecord,
    kind: MessageKind,
    show_thinking_texts: bool,
) -> bool {
    match kind {
        MessageKind::AgentText => true,
        // User input is the approved exception to the split's ACP/debug focus.
        MessageKind::UserInput => true,
        MessageKind::AgentThought => show_thinking_texts,
        MessageKind::Started | MessageKind::End => true,
        MessageKind::Brief | MessageKind::Summary | MessageKind::SummaryWarn => false,
    }
}
