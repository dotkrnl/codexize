use super::*;

fn content_line(n: usize) -> Line<'static> {
    Line::from(format!("Content line {}", n))
}

fn controls() -> Line<'static> {
    Line::from("Controls: [Space] expand | [q] quit")
}

const W: u16 = 80;

fn rule_width(line: &Line<'static>) -> usize {
    line.to_string().chars().filter(|c| *c == '─').count()
}

#[test]
fn zero_height() {
    let result = bottom_sheet(vec![content_line(1)], controls(), 0, W);
    assert_eq!(result.len(), 0);
}

#[test]
fn height_1_shows_only_controls() {
    let result = bottom_sheet(vec![content_line(1), content_line(2)], controls(), 1, W);
    assert_eq!(result.len(), 1);
    assert!(result[0].to_string().contains("Controls"));
}

#[test]
fn height_2_shows_rule_and_controls() {
    let result = bottom_sheet(vec![content_line(1), content_line(2)], controls(), 2, W);
    assert_eq!(result.len(), 2);
    assert!(result[0].to_string().contains('─'));
    assert!(result[1].to_string().contains("Controls"));
}

#[test]
fn all_content_fits() {
    let content = vec![content_line(1), content_line(2)];
    let result = bottom_sheet(content, controls(), 5, W);
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
    let result = bottom_sheet(content, controls(), 4, W);
    assert_eq!(result.len(), 4);
    assert!(result[0].to_string().contains('─'));
    assert!(result[1].to_string().contains("Content line 1"));
    assert!(result[2].to_string().contains('…'));
    assert!(result[3].to_string().contains("Controls"));
}

#[test]
fn exact_fit() {
    let content = vec![content_line(1), content_line(2), content_line(3)];
    let result = bottom_sheet(content, controls(), 5, W);
    assert_eq!(result.len(), 5);
    assert!(!result.iter().any(|l| l.to_string().contains('…')));
}

#[test]
fn height_10() {
    let content = vec![content_line(1), content_line(2)];
    let result = bottom_sheet(content, controls(), 10, W);
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
    let result = bottom_sheet(content, controls(), 3, W);
    assert_eq!(result.len(), 3);
    assert!(result[0].to_string().contains('─'));
    assert!(result[1].to_string().contains('…'));
    assert!(result[2].to_string().contains("Controls"));
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
    let result = bottom_sheet(content, controls(), 3, W);
    assert!(result.last().unwrap().to_string().contains("Controls"));
}

#[test]
fn empty_content() {
    let result = bottom_sheet(vec![], controls(), 5, W);
    assert_eq!(result.len(), 2);
    assert!(result[0].to_string().contains('─'));
    assert!(result[1].to_string().contains("Controls"));
}

#[test]
fn rule_spans_full_width_at_120() {
    let result = bottom_sheet(vec![content_line(1)], controls(), 5, 120);
    assert_eq!(rule_width(&result[0]), 120);
}

#[test]
fn rule_spans_full_width_at_200() {
    let result = bottom_sheet(vec![content_line(1)], controls(), 5, 200);
    assert_eq!(rule_width(&result[0]), 200);
}

#[test]
fn rule_spans_full_width_at_40() {
    let result = bottom_sheet(vec![content_line(1)], controls(), 5, 40);
    assert_eq!(rule_width(&result[0]), 40);
}

#[test]
fn rule_width_zero_collapses_rule() {
    let result = bottom_sheet(vec![content_line(1)], controls(), 5, 0);
    // Rule line still emitted (height-driven), but contains no glyphs.
    assert!(result[0].to_string().is_empty());
    assert!(result.last().unwrap().to_string().contains("Controls"));
}
