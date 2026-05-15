use crate::app_runtime::ModeFlags;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
/// Project + mode badges that anchor the left side of the top rule.
///
/// Built from a UI-neutral [`ModeFlags`] so the production draw path consumes
/// the runtime's [`crate::app_runtime::AppView`] for badge state rather than
/// reaching into pipeline state directly.
pub fn top_rule_left_spans_for(project: &str, modes: ModeFlags) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        project.to_string(),
        Style::default().fg(Color::DarkGray),
    )];
    if modes.yolo {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[YOLO]".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    if modes.cheap {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[CHEAP]".to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}
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
    format!("{truncated}…")
}
#[cfg(test)]
#[path = "top_rule_tests.rs"]
mod tests;
