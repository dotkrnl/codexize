use super::*;

#[test]
fn capitalize_first_handles_empty_and_ascii() {
    assert_eq!(capitalize_first(""), "");
    assert_eq!(capitalize_first("agent started"), "Agent started");
}

#[test]
fn interpolate_rgb_steps_between_colors() {
    assert_eq!(
        interpolate_rgb((0, 0, 0), (100, 200, 50), 2, 4),
        Color::Rgb(50, 100, 25)
    );
}

#[test]
fn gradient_spans_round_trip_text() {
    let spans = gradient_spans("work", 1);
    let text: String = spans.iter().map(|span| span.content.to_string()).collect();
    assert_eq!(text, "work");
    assert_eq!(spans.len(), 4);
}

#[test]
fn extract_short_title_prefers_prefix_before_pipe() {
    assert_eq!(
        extract_short_title("Working on tests | running cargo test"),
        "Working on tests"
    );
}
