#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChatScrollWindow {
    pub offset: usize,
    pub visible_end: usize,
    pub show_above_indicator: bool,
    pub show_below_indicator: bool,
    pub above_count: usize,
    pub below_count: usize,
}

pub(super) fn chat_scroll_window(
    total_lines: usize,
    available_height: usize,
    scroll_offset: usize,
) -> Option<ChatScrollWindow> {
    if total_lines == 0 || available_height == 0 {
        return None;
    }

    let has_overflow = total_lines > available_height;
    let max_offset = if has_overflow {
        total_lines.saturating_sub(available_height.saturating_sub(1))
    } else {
        0
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_window_reserves_rows_for_above_and_below_indicators() {
        let window = chat_scroll_window(10, 4, 3).unwrap();

        assert_eq!(window.offset, 3);
        assert_eq!(window.visible_end, 5);
        assert_eq!(window.above_count, 3);
        assert_eq!(window.below_count, 5);
        assert!(window.show_above_indicator);
        assert!(window.show_below_indicator);
    }

    #[test]
    fn chat_window_clamps_requested_offset_to_last_full_view() {
        let window = chat_scroll_window(10, 4, 99).unwrap();

        assert_eq!(window.offset, 7);
        assert_eq!(window.visible_end, 10);
        assert_eq!(window.below_count, 0);
        assert!(window.show_above_indicator);
        assert!(!window.show_below_indicator);
    }

    #[test]
    fn chat_window_returns_none_when_no_lines_can_render() {
        assert!(chat_scroll_window(0, 4, 0).is_none());
        assert!(chat_scroll_window(4, 0, 0).is_none());
    }
}
