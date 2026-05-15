#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChatScrollWindow {
    pub offset: usize,
    pub visible_end: usize,
    pub show_above_indicator: bool,
    pub show_below_indicator: bool,
    pub above_count: usize,
    pub below_count: usize,
}
pub(crate) fn chat_scroll_window(
    total_lines: usize,
    available_height: usize,
    scroll_offset: usize,
) -> Option<ChatScrollWindow> {
    if total_lines == 0 || available_height == 0 {
        return None;
    }
    let has_overflow = total_lines > available_height;
    let max_offset = compute_max_chat_scroll_offset(total_lines, available_height, has_overflow);
    let offset = scroll_offset.min(max_offset);
    let show_above_indicator = offset > 0;
    let mut message_rows = available_height.saturating_sub(show_above_indicator as usize);
    let show_below_indicator = total_lines > offset.saturating_add(message_rows);
    if show_below_indicator {
        message_rows = message_rows.saturating_sub(1);
    }
    let visible_end = (offset + message_rows).min(total_lines);
    Some(ChatScrollWindow {
        offset,
        visible_end,
        show_above_indicator,
        show_below_indicator,
        above_count: offset,
        below_count: total_lines.saturating_sub(visible_end),
    })
}
fn compute_max_chat_scroll_offset(
    total_lines: usize,
    available_height: usize,
    has_overflow: bool,
) -> usize {
    if has_overflow {
        total_lines.saturating_sub(available_height.saturating_sub(1))
    } else {
        0
    }
}
#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
