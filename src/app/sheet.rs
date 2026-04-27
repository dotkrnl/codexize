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
) -> Vec<Line<'static>> {
    let available = available_height as usize;

    if available == 0 {
        return vec![];
    }

    let rule_glyph = '─';
    let rule_style = Style::default().fg(Color::DarkGray);
    let rule_line = Line::from(Span::styled(
        // Use a reasonable width for the rule; actual width comes from terminal
        // In practice the caller should ensure proper width, but for testing use a default
        rule_glyph.to_string().repeat(80),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn content_line(n: usize) -> Line<'static> {
        Line::from(format!("Content line {}", n))
    }

    fn controls() -> Line<'static> {
        Line::from("Controls: [Space] expand | [q] quit")
    }

    #[test]
    fn zero_height() {
        let result = bottom_sheet(vec![content_line(1)], controls(), 0);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn height_1_shows_only_controls() {
        let result = bottom_sheet(vec![content_line(1), content_line(2)], controls(), 1);
        assert_eq!(result.len(), 1);
        assert!(result[0].to_string().contains("Controls"));
    }

    #[test]
    fn height_2_shows_rule_and_controls() {
        let result = bottom_sheet(vec![content_line(1), content_line(2)], controls(), 2);
        assert_eq!(result.len(), 2);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains("Controls"));
    }

    #[test]
    fn all_content_fits() {
        let content = vec![content_line(1), content_line(2)];
        let result = bottom_sheet(content, controls(), 5);
        // rule + 2 content + controls = 4 lines
        assert_eq!(result.len(), 4);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains("Content line 1"));
        assert!(result[2].to_string().contains("Content line 2"));
        assert!(result[3].to_string().contains("Controls"));
    }

    #[test]
    fn content_truncated_with_ellipsis() {
        let content = vec![
            content_line(1),
            content_line(2),
            content_line(3),
            content_line(4),
            content_line(5),
        ];
        let result = bottom_sheet(content, controls(), 4);
        // available_for_content = 4 - 2 = 2
        // Show 1 content line + ellipsis
        assert_eq!(result.len(), 4);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains("Content line 1"));
        assert!(result[2].to_string().contains('…'));
        assert!(result[3].to_string().contains("Controls"));
    }

    #[test]
    fn exact_fit() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 5);
        // rule + 3 content + controls = 5 lines
        assert_eq!(result.len(), 5);
        assert!(!result.iter().any(|l| l.to_string().contains('…')));
    }

    #[test]
    fn height_10() {
        let content = vec![content_line(1), content_line(2)];
        let result = bottom_sheet(content, controls(), 10);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn height_3_with_many_content() {
        let content = vec![
            content_line(1),
            content_line(2),
            content_line(3),
            content_line(4),
        ];
        let result = bottom_sheet(content, controls(), 3);
        // available_for_content = 3 - 2 = 1
        // Show ellipsis only (no content lines, since 1 - 1 = 0)
        assert_eq!(result.len(), 3);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains('…'));
        assert!(result[2].to_string().contains("Controls"));
    }

    // Snapshot tests at various available_height values
    #[test]
    fn snapshot_height_0() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 0);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn snapshot_height_1_controls_survive() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 1);
        // Controls line survives even when height is minimal
        assert_eq!(result.len(), 1);
        assert!(result[0].to_string().contains("Controls"));
    }

    #[test]
    fn snapshot_height_2_rule_and_controls() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 2);
        assert_eq!(result.len(), 2);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains("Controls"));
    }

    #[test]
    fn snapshot_height_3_ellipsis() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 3);
        // rule + ellipsis + controls
        assert_eq!(result.len(), 3);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains('…'));
        assert!(result[2].to_string().contains("Controls"));
    }

    #[test]
    fn snapshot_height_4_one_content() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 4);
        // available_for_content = 4 - 2 = 2
        // Take 1 content + ellipsis
        assert_eq!(result.len(), 4);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains("Content line 1"));
        assert!(result[2].to_string().contains('…'));
        assert!(result[3].to_string().contains("Controls"));
    }

    #[test]
    fn snapshot_height_5_all_content_fits() {
        let content = vec![content_line(1), content_line(2), content_line(3)];
        let result = bottom_sheet(content, controls(), 5);
        // rule + 3 content + controls (exactly fits)
        assert_eq!(result.len(), 5);
        assert!(result[1].to_string().contains("Content line 1"));
        assert!(result[2].to_string().contains("Content line 2"));
        assert!(result[3].to_string().contains("Content line 3"));
        assert!(!result.iter().any(|l| l.to_string().contains('…')));
    }

    #[test]
    fn snapshot_height_6_with_truncation() {
        let content = vec![content_line(1), content_line(2), content_line(3), content_line(4), content_line(5)];
        let result = bottom_sheet(content, controls(), 6);
        // rule + 3 content + ellipsis + controls
        assert_eq!(result.len(), 6);
        assert!(result[1].to_string().contains("Content line 1"));
        assert!(result[2].to_string().contains("Content line 2"));
        assert!(result[3].to_string().contains("Content line 3"));
        assert!(result[4].to_string().contains('…'));
    }

    #[test]
    fn snapshot_height_10_all_content() {
        let content = vec![content_line(1), content_line(2)];
        let result = bottom_sheet(content, controls(), 10);
        // rule + 2 content + controls
        assert_eq!(result.len(), 4);
        assert!(!result.iter().any(|l| l.to_string().contains('…')));
    }

    #[test]
    fn controls_never_sacrificed_with_many_content() {
        let content = vec![
            content_line(1),
            content_line(2),
            content_line(3),
            content_line(4),
            content_line(5),
            content_line(6),
            content_line(7),
            content_line(8),
        ];
        let result = bottom_sheet(content, controls(), 3);
        // Even with 8 content lines and height 3, controls survive
        assert!(result.last().unwrap().to_string().contains("Controls"));
    }

    #[test]
    fn empty_content() {
        let result = bottom_sheet(vec![], controls(), 5);
        // rule + controls (no content)
        assert_eq!(result.len(), 2);
        assert!(result[0].to_string().contains('─'));
        assert!(result[1].to_string().contains("Controls"));
    }
}
