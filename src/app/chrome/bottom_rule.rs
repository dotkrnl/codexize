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
        return Line::from(Span::styled(rule_glyph.to_string().repeat(width), rule_style));
    };

    let badge_text = badge.format_text();
    let badge_len = badge_text.chars().count();

    if badge_len >= width {
        // Badge too wide, just show the rule
        return Line::from(Span::styled(rule_glyph.to_string().repeat(width), rule_style));
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
mod tests {
    use super::*;

    #[test]
    fn no_badge() {
        let line = bottom_rule(80, None);
        let text = line.to_string();
        assert!(text.contains('─'));
        assert!(!text.contains('↓'));
    }

    #[test]
    fn with_badge() {
        let line = bottom_rule(80, Some(UnreadBadge { count: 5 }));
        let text = line.to_string();
        assert!(text.contains('↓'));
        assert!(text.contains("5 new"));
    }

    #[test]
    fn badge_centered() {
        let line = bottom_rule(40, Some(UnreadBadge { count: 3 }));
        // Badge is " ↓ 3 new " = 9 chars
        // Left rule: (40 - 9) / 2 = 15
        // Right rule: 40 - 9 - 15 = 16
        let spans = &line.spans;
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.chars().count(), 15);
        assert_eq!(spans[1].content, " ↓ 3 new ");
        assert_eq!(spans[2].content.chars().count(), 16);
    }

    #[test]
    fn badge_too_wide() {
        let line = bottom_rule(5, Some(UnreadBadge { count: 999 }));
        let text = line.to_string();
        // Badge " ↓ 999 new " is 11 chars, too wide for width 5
        // Should just show rule
        assert!(!text.contains('↓'));
    }

    #[test]
    fn zero_width() {
        let line = bottom_rule(0, Some(UnreadBadge { count: 5 }));
        assert_eq!(line.spans.len(), 0);
    }

    // Snapshot tests at representative widths
    #[test]
    fn snapshot_width_200_no_badge() {
        let line = bottom_rule(200, None);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.to_string().chars().filter(|&c| c == '─').count(), 200);
    }

    #[test]
    fn snapshot_width_200_with_badge() {
        let line = bottom_rule(200, Some(UnreadBadge { count: 15 }));
        let spans = &line.spans;
        assert_eq!(spans.len(), 3);
        assert!(line.to_string().contains("↓ 15 new"));
        // Badge " ↓ 15 new " = 10 chars
        let left = spans[0].content.chars().count();
        let right = spans[2].content.chars().count();
        assert_eq!(left + 10 + right, 200);
    }

    #[test]
    fn snapshot_width_120_with_badge() {
        let line = bottom_rule(120, Some(UnreadBadge { count: 42 }));
        assert!(line.to_string().contains("↓ 42 new"));
        let spans = &line.spans;
        assert_eq!(spans.len(), 3);
        // Verify centering
        let badge_len = spans[1].content.chars().count();
        let left_len = spans[0].content.chars().count();
        let right_len = spans[2].content.chars().count();
        assert_eq!(left_len + badge_len + right_len, 120);
    }

    #[test]
    fn snapshot_width_80_no_badge() {
        let line = bottom_rule(80, None);
        assert_eq!(line.to_string().chars().filter(|&c| c == '─').count(), 80);
    }

    #[test]
    fn snapshot_width_80_with_badge() {
        let line = bottom_rule(80, Some(UnreadBadge { count: 7 }));
        assert!(line.to_string().contains("↓ 7 new"));
    }

    #[test]
    fn snapshot_width_60_with_badge() {
        let line = bottom_rule(60, Some(UnreadBadge { count: 10 }));
        assert!(line.to_string().contains("↓ 10 new"));
        let spans = &line.spans;
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn snapshot_width_40_no_badge() {
        let line = bottom_rule(40, None);
        assert_eq!(line.to_string().chars().filter(|&c| c == '─').count(), 40);
    }

    #[test]
    fn snapshot_width_40_with_badge() {
        let line = bottom_rule(40, Some(UnreadBadge { count: 3 }));
        let spans = &line.spans;
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].content, " ↓ 3 new ");
    }

    #[test]
    fn snapshot_width_30_no_badge() {
        let line = bottom_rule(30, None);
        assert_eq!(line.to_string().chars().filter(|&c| c == '─').count(), 30);
    }

    #[test]
    fn snapshot_width_30_with_badge() {
        let line = bottom_rule(30, Some(UnreadBadge { count: 2 }));
        assert!(line.to_string().contains("↓ 2 new"));
    }

    #[test]
    fn badge_odd_width_centering() {
        // Test centering with odd-width terminals
        let line = bottom_rule(41, Some(UnreadBadge { count: 5 }));
        let spans = &line.spans;
        assert_eq!(spans.len(), 3);
        // Badge " ↓ 5 new " = 9 chars
        // Left: (41 - 9) / 2 = 16
        // Right: 41 - 9 - 16 = 16
        assert_eq!(spans[0].content.chars().count(), 16);
        assert_eq!(spans[2].content.chars().count(), 16);
    }

    #[test]
    fn badge_even_width_centering() {
        let line = bottom_rule(42, Some(UnreadBadge { count: 5 }));
        let spans = &line.spans;
        // Badge " ↓ 5 new " = 9 chars
        // Left: (42 - 9) / 2 = 16
        // Right: 42 - 9 - 16 = 17
        assert_eq!(spans[0].content.chars().count(), 16);
        assert_eq!(spans[2].content.chars().count(), 17);
    }
}
