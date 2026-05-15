use super::*;

#[test]
fn both_fit_with_generous_separator() {
    let line = top_rule("myproject", Some("Agent · Processing"), 200);
    let text = line.to_string();
    assert!(text.contains("myproject"));
    assert!(text.contains("Agent · Processing"));
}

#[test]
fn text_segments_are_dimmed() {
    let line = top_rule("myproject", Some("Agent"), 80);
    for span in line.spans.iter().filter(|span| !span.content.contains('─')) {
        assert_eq!(span.style.fg, Some(Color::DarkGray));
    }
}

#[test]
fn both_fit_with_minimum_separator() {
    let line = top_rule("myproject", Some("paused"), 38);
    let text = line.to_string();
    assert!(text.contains("myproject"));
    assert!(text.contains("paused"));
}

#[test]
fn untruncated_right_always_renders_even_if_short() {
    // "paused" is 6 chars (< 8), but untruncated, so should render
    let line = top_rule("myproject", Some("paused"), 40);
    let text = line.to_string();
    assert!(text.contains("paused"));
}

#[test]
fn truncated_right_below_8_cols_is_dropped() {
    // Width 17: left=9, right needs to truncate to <8 cols, so drop it.
    let line = top_rule("myproject", Some("Agent · Very Long Processing Title"), 17);
    let text = line.to_string();
    assert!(text.contains("myproject"));
    assert!(!text.contains("Agent"));
}

#[test]
fn right_truncated_with_ellipsis() {
    let line = top_rule("myproject", Some("Agent · Very Long Processing Title"), 18);
    let text = line.to_string();
    assert!(text.contains("myproject"));
    assert!(text.contains("…"));
}

#[test]
fn left_text_truncates_with_ellipsis() {
    let line = top_rule("very-long-project-name", None, 12);
    let text = line.to_string();
    assert!(text.contains("…"));
}

#[test]
fn zero_width() {
    let line = top_rule("project", Some("right"), 0);
    assert_eq!(line.spans.len(), 0);
}

#[test]
fn no_right_segment() {
    let line = top_rule("myproject", None, 80);
    let text = line.to_string();
    assert!(text.contains("myproject"));
    assert!(text.contains('─'));
}

#[test]
fn styled_left_spans_preserve_badge_style() {
    let line = top_rule_with_left_spans(
        vec![
            Span::styled("codexize".to_string(), Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                "[CHEAP]".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ],
        Some("running"),
        80,
    );

    let cheap = line
        .spans
        .iter()
        .find(|span| span.content == "[CHEAP]")
        .expect("cheap badge span");
    assert_eq!(cheap.style.fg, Some(Color::Green));
    assert!(
        cheap
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn edge_case_right_exactly_8_cols_after_truncate() {
    // Test the boundary: truncated right exactly 8 cols should render
    let line = top_rule("left", Some("12345678901234567890"), 12);
    let text = line.to_string();
    // Available for right: 12 - 4 - 1 = 7, which is < 8, so drop
    assert!(!text.contains("123456"));
}

#[test]
fn edge_case_right_9_cols_after_truncate() {
    // 9 cols is >= 8, should render with ellipsis
    let line = top_rule("left", Some("1234567890123456"), 14);
    let text = line.to_string();
    // Available for right: 14 - 4 - 1 = 9, which is >= 8
    assert!(text.contains("…"));
}

#[test]
fn long_left_text_truncates_cleanly() {
    let line = top_rule("very-very-long-project-name", None, 12);
    let text = line.to_string();
    assert!(text.contains("…"));
}

#[test]
fn tiny_width_still_renders_something() {
    let line = top_rule("project", None, 3);
    assert!(!line.to_string().is_empty());
}
