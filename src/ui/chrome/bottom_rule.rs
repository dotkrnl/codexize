use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
/// Badge shown when tail-follow is detached and unread content exists below viewport.
#[derive(Clone, Debug)]
pub struct UnreadBadge {
    pub count: usize,
}
impl UnreadBadge {
    /// Format as "↓ N new" with spaces for padding.
    fn format_text(&self) -> String {
        format!(" ↓ {} new ", self.count)
    }
}
/// Renders a full-width horizontal rule with an optional centered badge overlay.
///
/// The badge (when present) is rendered inline, overlaying the rule glyphs at the
/// centered column with yellow background and bold black text.
pub fn bottom_rule(width: u16, badge: Option<UnreadBadge>) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from(vec![]);
    }
    let rule_glyph = '─';
    let rule_style = Style::default().fg(Color::DarkGray);
    let Some(badge) = badge else {
        // No badge: just a full-width rule
        return Line::from(Span::styled(
            rule_glyph.to_string().repeat(width),
            rule_style,
        ));
    };
    let badge_text = badge.format_text();
    let badge_len = badge_text.chars().count();
    if badge_len >= width {
        // Badge too wide, just show the rule
        return Line::from(Span::styled(
            rule_glyph.to_string().repeat(width),
            rule_style,
        ));
    }
    // Center the badge
    let left_rule_len = (width - badge_len) / 2;
    let right_rule_len = width - badge_len - left_rule_len;
    let badge_style = Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(rule_glyph.to_string().repeat(left_rule_len), rule_style),
        Span::styled(badge_text, badge_style),
        Span::styled(rule_glyph.to_string().repeat(right_rule_len), rule_style),
    ])
}
#[cfg(test)]
#[path = "bottom_rule_tests.rs"]
mod tests;
