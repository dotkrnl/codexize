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
