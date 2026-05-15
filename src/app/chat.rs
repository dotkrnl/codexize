pub(crate) fn max_chat_scroll_offset(body_height: usize, message_count: usize) -> usize {
    if message_count == 0 || message_count <= body_height {
        0
    } else {
        message_count - body_height
    }
}
pub(crate) fn transcript_line_count(
    messages: &[crate::state::Message],
    run: &crate::state::RunRecord,
    available_width: usize,
    include_running_tail: bool,
) -> usize {
    let width = available_width.max(1);
    let message_lines = messages
        .iter()
        .map(|message| {
            // Keep app-side scroll math UI-neutral while matching the TUI's
            // one header row for transcript-style message bodies.
            let wrapped = crate::app::render_helpers::wrap_text(&message.text, width)
                .len()
                .max(1);
            let header = matches!(
                message.kind,
                crate::state::MessageKind::UserInput
                    | crate::state::MessageKind::AgentText
                    | crate::state::MessageKind::AgentThought
            ) as usize;
            wrapped + header
        })
        .sum::<usize>();
    let has_end = messages
        .iter()
        .any(|message| message.kind == crate::state::MessageKind::End);
    let tail_lines = usize::from(
        include_running_tail && run.status == crate::state::RunStatus::Running && !has_end,
    );
    message_lines + tail_lines
}
