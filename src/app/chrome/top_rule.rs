use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Renders a full-width horizontal rule with text overlaid at left and right edges.
///
/// Left segment is the anchor (project name); right segment is truncated first.
#[cfg(test)]
fn top_rule(left_text: &str, right_text_opt: Option<&str>, width: u16) -> Line<'static> {
    let text_style = Style::default().fg(Color::DarkGray);
    top_rule_with_left_spans(
        vec![Span::styled(left_text.to_string(), text_style)],
        right_text_opt,
        width,
    )
}

pub fn top_rule_with_left_spans(
    left_spans: Vec<Span<'static>>,
    right_text_opt: Option<&str>,
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from(vec![]);
    }

    let rule_glyph = '─';
    let rule_style = Style::default().fg(Color::DarkGray);
    let text_style = Style::default().fg(Color::DarkGray);

    let Some(right_text) = right_text_opt else {
        // No right segment: just fill with rule and overlay left
        return overlay_left_spans_on_rule(left_spans, width, rule_glyph, rule_style);
    };

    let left_len = spans_width(&left_spans);
    let right_len = right_text.chars().count();

    // If both fit with at least 4 cols of separator, render both
    if left_len + right_len + 4 <= width {
        let separator_len = width - left_len - right_len;
        let mut spans = left_spans;
        spans.push(Span::styled(
            rule_glyph.to_string().repeat(separator_len),
            rule_style,
        ));
        spans.push(Span::styled(right_text.to_string(), text_style));
        return Line::from(spans);
    }

    // Untruncated right segment always renders even if short
    if right_len + left_len < width {
        let separator_len = width - left_len - right_len;
        let mut spans = left_spans;
        spans.push(Span::styled(
            rule_glyph.to_string().repeat(separator_len),
            rule_style,
        ));
        spans.push(Span::styled(right_text.to_string(), text_style));
        return Line::from(spans);
    }

    // Try truncating right segment with ellipsis until it fits with 1 col separator
    // If truncated right would have <8 visible cols, drop it
    let max_right_with_separator = width.saturating_sub(left_len + 1);

    if max_right_with_separator >= 8 {
        // Right segment can fit with truncation
        let truncated_right = truncate_with_ellipsis(right_text, max_right_with_separator);
        let truncated_len = truncated_right.chars().count();
        let separator_len = width - left_len - truncated_len;

        let mut spans = left_spans;
        spans.push(Span::styled(
            rule_glyph.to_string().repeat(separator_len),
            rule_style,
        ));
        spans.push(Span::styled(truncated_right, text_style));
        return Line::from(spans);
    }

    // Right segment would be <8 cols after truncation, drop it
    // Now just render left, truncating if needed
    overlay_left_spans_on_rule(left_spans, width, rule_glyph, rule_style)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

fn spans_plain_text(spans: &[Span<'_>]) -> String {
    spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
}

/// Overlay left text on a full-width rule, truncating left if needed.
fn overlay_left_spans_on_rule(
    left_spans: Vec<Span<'static>>,
    width: usize,
    rule_glyph: char,
    rule_style: Style,
) -> Line<'static> {
    let left_len = spans_width(&left_spans);
    let text_style = Style::default().fg(Color::DarkGray);

    if left_len <= width {
        let rule_len = width - left_len;
        let mut spans = left_spans;
        spans.push(Span::styled(
            rule_glyph.to_string().repeat(rule_len),
            rule_style,
        ));
        return Line::from(spans);
    }

    let left_text = spans_plain_text(&left_spans);
    let truncated = truncate_with_ellipsis(&left_text, width);
    let truncated_len = truncated.chars().count();
    let rule_len = width.saturating_sub(truncated_len);

    Line::from(vec![
        Span::styled(truncated, text_style),
        Span::styled(rule_glyph.to_string().repeat(rule_len), rule_style),
    ])
}

/// Truncate text to max_len chars, adding '…' if truncated.
fn truncate_with_ellipsis(text: &str, max_len: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_len {
        return text.to_string();
    }

    if max_len == 0 {
        return String::new();
    }

    if max_len == 1 {
        return "…".to_string();
    }

    // Take max_len - 1 chars and add ellipsis
    let truncated: String = text.chars().take(max_len - 1).collect();
    format!("{}…", truncated)
}

#[cfg(test)]
mod tests {
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
                        .add_modifier(ratatui::style::Modifier::BOLD),
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
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    // Snapshot tests at representative widths
    #[test]
    fn snapshot_width_200() {
        let line = top_rule(
            "codexize",
            Some("Reviewer · Evaluating implementation quality"),
            200,
        );
        // Both should fit with generous separator
        assert!(line.to_string().contains("codexize"));
        assert!(
            line.to_string()
                .contains("Reviewer · Evaluating implementation quality")
        );
        // Should have at least 4 cols separator
        assert!(line.spans.len() == 3);
        assert!(line.spans[1].content.chars().count() >= 4);
    }

    #[test]
    fn snapshot_width_120() {
        let line = top_rule("codexize", Some("Implementation R2 · running"), 120);
        assert!(line.to_string().contains("codexize"));
        assert!(line.to_string().contains("Implementation R2 · running"));
    }

    #[test]
    fn snapshot_width_80() {
        let line = top_rule("codexize", Some("Spec Review · awaiting input"), 80);
        assert!(line.to_string().contains("codexize"));
        assert!(line.to_string().contains("Spec Review · awaiting input"));
    }

    #[test]
    fn snapshot_width_60_truncated_right() {
        let line = top_rule(
            "codexize",
            Some("Very Long Agent Name · Processing complex task with extra detail"),
            60,
        );
        let text = line.to_string();
        assert!(text.contains("codexize"));
        // Right should be truncated with ellipsis
        assert!(text.contains("…"));
    }

    #[test]
    fn snapshot_width_40_short_right() {
        let line = top_rule("codexize", Some("paused"), 40);
        let text = line.to_string();
        assert!(text.contains("codexize"));
        assert!(text.contains("paused"));
    }

    #[test]
    fn snapshot_width_16_left_only() {
        let line = top_rule("codexize", Some("Agent · Processing"), 16);
        let text = line.to_string();
        // Right segment should be dropped (would be <8 cols after truncation).
        assert!(!text.contains("Agent"));
        assert!(text.contains("codexize"));
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
}
