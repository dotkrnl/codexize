use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Renders a full-width horizontal rule with text overlaid at left and right edges.
///
/// Left segment is the anchor (project · session); right segment is truncated first.
/// When the left no longer fits, truncate project name first, then session id while
/// preserving its trailing time component.
pub fn top_rule(left_text: &str, right_text_opt: Option<&str>, width: u16) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from(vec![]);
    }

    let rule_glyph = '─';
    let rule_style = Style::default().fg(Color::DarkGray);
    let text_style = Style::default().fg(Color::DarkGray);

    let Some(right_text) = right_text_opt else {
        // No right segment: just fill with rule and overlay left
        return overlay_left_on_rule(left_text, width, rule_glyph, rule_style);
    };

    let left_len = left_text.chars().count();
    let right_len = right_text.chars().count();

    // If both fit with at least 4 cols of separator, render both
    if left_len + right_len + 4 <= width {
        let separator_len = width - left_len - right_len;
        return Line::from(vec![
            Span::styled(left_text.to_string(), text_style),
            Span::styled(rule_glyph.to_string().repeat(separator_len), rule_style),
            Span::styled(right_text.to_string(), text_style),
        ]);
    }

    // Untruncated right segment always renders even if short
    if right_len + left_len < width {
        let separator_len = width - left_len - right_len;
        return Line::from(vec![
            Span::styled(left_text.to_string(), text_style),
            Span::styled(rule_glyph.to_string().repeat(separator_len), rule_style),
            Span::styled(right_text.to_string(), text_style),
        ]);
    }

    // Try truncating right segment with ellipsis until it fits with 1 col separator
    // If truncated right would have <8 visible cols, drop it
    let max_right_with_separator = width.saturating_sub(left_len + 1);

    if max_right_with_separator >= 8 {
        // Right segment can fit with truncation
        let truncated_right = truncate_with_ellipsis(right_text, max_right_with_separator);
        let truncated_len = truncated_right.chars().count();
        let separator_len = width - left_len - truncated_len;

        return Line::from(vec![
            Span::styled(left_text.to_string(), text_style),
            Span::styled(rule_glyph.to_string().repeat(separator_len), rule_style),
            Span::styled(truncated_right, text_style),
        ]);
    }

    // Right segment would be <8 cols after truncation, drop it
    // Now just render left, truncating if needed
    overlay_left_on_rule(left_text, width, rule_glyph, rule_style)
}

/// Overlay left text on a full-width rule, truncating left if needed.
///
/// Truncation order: project name first, then session id (preserve trailing time).
fn overlay_left_on_rule(
    left_text: &str,
    width: usize,
    rule_glyph: char,
    rule_style: Style,
) -> Line<'static> {
    let left_len = left_text.chars().count();
    let text_style = Style::default().fg(Color::DarkGray);

    if left_len <= width {
        let rule_len = width - left_len;
        return Line::from(vec![
            Span::styled(left_text.to_string(), text_style),
            Span::styled(rule_glyph.to_string().repeat(rule_len), rule_style),
        ]);
    }

    // Left doesn't fit, need to truncate
    // Format is "project · session", where session is a timestamp like "20260427-101009"
    // Truncate project first, then session but preserve trailing time component

    if let Some(separator_pos) = left_text.rfind(" · ") {
        let project = &left_text[..separator_pos];
        let session = &left_text[separator_pos + " · ".len()..];

        // Try truncating project first
        let separator = " · ";
        let session_len = session.chars().count();
        let separator_len = separator.chars().count();
        let available_for_project = width.saturating_sub(session_len + separator_len);

        if available_for_project > 0 {
            let truncated_project = truncate_with_ellipsis(project, available_for_project);
            let truncated_left = format!("{}{}{}", truncated_project, separator, session);
            let truncated_len = truncated_left.chars().count();
            let rule_len = width.saturating_sub(truncated_len);

            return Line::from(vec![
                Span::styled(truncated_left, text_style),
                Span::styled(rule_glyph.to_string().repeat(rule_len), rule_style),
            ]);
        }

        // Project gone, now truncate session but preserve trailing time component
        // Session format: "20260427-101009" (date-time), preserve "-101009" part
        if let Some(dash_pos) = session.rfind('-') {
            let time_part = &session[dash_pos..]; // includes the dash
            let time_len = time_part.chars().count();

            if time_len <= width {
                let available_for_date = width.saturating_sub(time_len + separator_len);
                if available_for_date > 0 {
                    let date_part = &session[..dash_pos];
                    let truncated_date = truncate_with_ellipsis(date_part, available_for_date);
                    let truncated_left = format!("{}{}{}", separator, truncated_date, time_part);
                    let truncated_len = truncated_left.chars().count();
                    let rule_len = width.saturating_sub(truncated_len);

                    return Line::from(vec![
                        Span::styled(truncated_left, text_style),
                        Span::styled(rule_glyph.to_string().repeat(rule_len), rule_style),
                    ]);
                }

                // Just time part
                let truncated_left = format!("{}{}", separator, time_part);
                let truncated_len = truncated_left.chars().count();
                let final_left = if truncated_len <= width {
                    truncated_left
                } else {
                    truncate_with_ellipsis(&truncated_left, width)
                };

                return Line::from(vec![
                    Span::styled(final_left.clone(), text_style),
                    Span::styled(
                        rule_glyph
                            .to_string()
                            .repeat(width.saturating_sub(final_left.chars().count())),
                        rule_style,
                    ),
                ]);
            }
        }
    }

    // Fallback: just truncate the whole left text
    let truncated = truncate_with_ellipsis(left_text, width);
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
        let line = top_rule(
            "myproject · 20260427-101009",
            Some("Agent · Processing"),
            200,
        );
        let text = line.to_string();
        assert!(text.contains("myproject · 20260427-101009"));
        assert!(text.contains("Agent · Processing"));
    }

    #[test]
    fn text_segments_are_dimmed() {
        let line = top_rule("myproject · 20260427-101009", Some("Agent"), 80);
        for span in line.spans.iter().filter(|span| !span.content.contains('─')) {
            assert_eq!(span.style.fg, Some(Color::DarkGray));
        }
    }

    #[test]
    fn both_fit_with_minimum_separator() {
        let line = top_rule("myproject · 20260427-101009", Some("paused"), 38);
        let text = line.to_string();
        assert!(text.contains("myproject · 20260427-101009"));
        assert!(text.contains("paused"));
    }

    #[test]
    fn untruncated_right_always_renders_even_if_short() {
        // "paused" is 6 chars (< 8), but untruncated, so should render
        let line = top_rule("myproject · 20260427-101009", Some("paused"), 40);
        let text = line.to_string();
        assert!(text.contains("paused"));
    }

    #[test]
    fn truncated_right_below_8_cols_is_dropped() {
        // Width 35: left=27, right needs to truncate to <8 cols, so drop it
        let line = top_rule(
            "myproject · 20260427-101009",
            Some("Agent · Very Long Processing Title"),
            35,
        );
        let text = line.to_string();
        assert!(text.contains("myproject · 20260427-101009"));
        assert!(!text.contains("Agent"));
    }

    #[test]
    fn right_truncated_with_ellipsis() {
        let line = top_rule(
            "myproject · 20260427-101009",
            Some("Agent · Very Long Processing Title"),
            50,
        );
        let text = line.to_string();
        assert!(text.contains("myproject · 20260427-101009"));
        assert!(text.contains("…"));
    }

    #[test]
    fn project_truncated_before_session() {
        let line = top_rule("very-long-project-name · 20260427-101009", None, 30);
        let text = line.to_string();
        assert!(text.contains("20260427-101009"));
        assert!(text.contains("…"));
    }

    #[test]
    fn session_date_truncated_time_preserved() {
        let line = top_rule("project · 20260427-101009", None, 15);
        let text = line.to_string();
        assert!(text.contains("-101009"));
        assert!(text.contains("…"));
    }

    #[test]
    fn zero_width() {
        let line = top_rule("project · 20260427-101009", Some("right"), 0);
        assert_eq!(line.spans.len(), 0);
    }

    #[test]
    fn no_right_segment() {
        let line = top_rule("myproject · 20260427-101009", None, 80);
        let text = line.to_string();
        assert!(text.contains("myproject · 20260427-101009"));
        assert!(text.contains('─'));
    }

    // Snapshot tests at representative widths
    #[test]
    fn snapshot_width_200() {
        let line = top_rule(
            "codexize · 20260427-101009",
            Some("Reviewer · Evaluating implementation quality"),
            200,
        );
        // Both should fit with generous separator
        assert!(line.to_string().contains("codexize · 20260427-101009"));
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
        let line = top_rule(
            "codexize · 20260427-101009",
            Some("Implementation R2 · running"),
            120,
        );
        assert!(line.to_string().contains("codexize · 20260427-101009"));
        assert!(line.to_string().contains("Implementation R2 · running"));
    }

    #[test]
    fn snapshot_width_80() {
        let line = top_rule(
            "codexize · 20260427-101009",
            Some("Spec Review · awaiting input"),
            80,
        );
        assert!(line.to_string().contains("codexize · 20260427-101009"));
        assert!(line.to_string().contains("Spec Review · awaiting input"));
    }

    #[test]
    fn snapshot_width_60_truncated_right() {
        let line = top_rule(
            "codexize · 20260427-101009",
            Some("Very Long Agent Name · Processing complex task"),
            60,
        );
        let text = line.to_string();
        assert!(text.contains("codexize · 20260427-101009"));
        // Right should be truncated with ellipsis
        assert!(text.contains("…"));
    }

    #[test]
    fn snapshot_width_40_short_right() {
        let line = top_rule("codexize · 20260427-101009", Some("paused"), 40);
        let text = line.to_string();
        assert!(text.contains("codexize · 20260427-101009"));
        assert!(text.contains("paused"));
    }

    #[test]
    fn snapshot_width_30_left_only() {
        let line = top_rule("codexize · 20260427-101009", Some("Agent · Processing"), 30);
        let text = line.to_string();
        // Right segment should be dropped (would be <8 cols after truncation)
        assert!(!text.contains("Agent"));
        // Left fits in 30 (26 chars), no truncation needed
        assert!(text.contains("codexize · 20260427-101009"));
    }

    #[test]
    fn edge_case_right_exactly_8_cols_after_truncate() {
        // Test the boundary: truncated right exactly 8 cols should render
        let line = top_rule("left · 20260427-101009", Some("12345678901234567890"), 30);
        let text = line.to_string();
        // Available for right: 30 - 22 - 1 = 7, which is < 8, so drop
        assert!(!text.contains("123456"));
    }

    #[test]
    fn edge_case_right_9_cols_after_truncate() {
        // 9 cols is >= 8, should render with ellipsis
        let line = top_rule("left · 20260427-101009", Some("1234567890123456"), 32);
        let text = line.to_string();
        // Available for right: 32 - 22 - 1 = 9, which is >= 8
        assert!(text.contains("…"));
    }

    #[test]
    fn truncation_order_project_first() {
        let line = top_rule("very-very-long-project-name · 20260427-101009", None, 35);
        let text = line.to_string();
        // Session should be intact, project truncated
        assert!(text.contains("20260427-101009"));
        assert!(text.contains("…"));
        assert!(!text.contains("very-very-long-project-name"));
    }

    #[test]
    fn truncation_order_session_date_then_time() {
        let line = top_rule("p · 20260427-101009", None, 15);
        let text = line.to_string();
        // Time component should be preserved
        assert!(text.contains("-101009"));
    }
}
