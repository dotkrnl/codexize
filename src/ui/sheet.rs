use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Renders a bottom sheet filling the footer zone.
///
/// Returns lines beginning with a leading dark-gray rule, followed by content,
/// then the controls line. The controls line is never sacrificed: if available_height
/// is too small, content is truncated with ellipsis but the rule + controls line
/// remain intact.
pub fn bottom_sheet(
    content_lines: Vec<Line<'static>>,
    controls_line: Line<'static>,
    available_height: u16,
    width: u16,
) -> Vec<Line<'static>> {
    let available = available_height as usize;

    if available == 0 {
        return vec![];
    }

    let rule_glyph = '─';
    let rule_style = Style::default().fg(Color::DarkGray);
    let rule_line = Line::from(Span::styled(
        rule_glyph.to_string().repeat(width as usize),
        rule_style,
    ));

    // Minimum: rule + controls line
    let minimum_lines = 2;

    if available < minimum_lines {
        // Not enough space for even rule + controls
        // Controls line replaces everything
        return vec![controls_line];
    }

    if available == minimum_lines {
        // Exactly enough for rule + controls, no content
        return vec![rule_line, controls_line];
    }

    // available > 2, so we have: rule + some content + controls
    let available_for_content = available - 2; // -1 for rule, -1 for controls

    if content_lines.len() <= available_for_content {
        // All content fits
        let mut result = vec![rule_line];
        result.extend(content_lines);
        result.push(controls_line);
        return result;
    }

    // Need to truncate content with ellipsis
    let mut truncated_content: Vec<Line<'static>> = content_lines
        .into_iter()
        .take(available_for_content.saturating_sub(1))
        .collect();

    // Add ellipsis line
    let ellipsis_line = Line::from(Span::styled("…", Style::default().fg(Color::DarkGray)));
    truncated_content.push(ellipsis_line);

    let mut result = vec![rule_line];
    result.extend(truncated_content);
    result.push(controls_line);
    result
}

/// Renders bottom-sheet content when the surrounding chrome already provides
/// the divider rule.
pub fn bottom_sheet_without_rule(
    content_lines: Vec<Line<'static>>,
    controls_line: Line<'static>,
    available_height: u16,
) -> Vec<Line<'static>> {
    let available = available_height as usize;

    if available == 0 {
        return vec![];
    }

    if available == 1 {
        return vec![controls_line];
    }

    let available_for_content = available - 1;

    if content_lines.len() <= available_for_content {
        let mut result = content_lines;
        result.push(controls_line);
        return result;
    }

    let mut truncated_content: Vec<Line<'static>> = content_lines
        .into_iter()
        .take(available_for_content.saturating_sub(1))
        .collect();
    truncated_content.push(Line::from(Span::styled(
        "…",
        Style::default().fg(Color::DarkGray),
    )));
    truncated_content.push(controls_line);
    truncated_content
}

#[cfg(test)]
#[path = "sheet_tests.rs"]
mod tests;
