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
