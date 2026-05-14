use super::*;
use crate::app::clock::TestClock;
use std::time::{Duration, SystemTime};

fn line_text(line: &Line) -> String {
    line.spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<String>()
}

#[test]
fn historical_message_format() {
    let line = format_historical_message(
        "14:30:25",
        "○",
        "agent started",
        Color::DarkGray,
        HistoricalStyleHints::default(),
    );
    let text = line_text(&line);
    assert_eq!(text, "14:30:25 ○ Agent started");
}

#[test]
fn historical_message_summary_style() {
    let line = format_historical_message(
        "14:30:25",
        "✓",
        "completed successfully",
        Color::Green,
        HistoricalStyleHints {
            is_summary: true,
            ..Default::default()
        },
    );
    let body_span = &line.spans[2];
    assert_eq!(body_span.style.fg, Some(Color::Green));
}

#[test]
fn historical_message_error_style() {
    let line = format_historical_message(
        "14:30:25",
        "✗",
        "failed",
        Color::Red,
        HistoricalStyleHints {
            is_error: true,
            ..Default::default()
        },
    );
    let body_span = &line.spans[2];
    assert_eq!(body_span.style.fg, Some(Color::Red));
}

#[test]
fn running_message_1hz_clock_stable_within_second() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("working on task".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_a = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let line_b = format_running_transcript_leaf(marker, &clock, 0, &fetcher);

    let text_a = line_text(&line_a);
    let text_b = line_text(&line_b);

    assert_eq!(
        text_a, text_b,
        "same-second renders should be byte-identical"
    );
}

#[test]
fn running_message_1hz_clock_differs_across_second() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("working on task".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_before = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_before = line_text(&line_before);

    clock.advance(Duration::from_secs(1));

    let line_after = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_after = line_text(&line_after);

    assert_ne!(
        text_before, text_after,
        "timestamp must differ after crossing a second boundary"
    );
}

#[test]
fn running_message_1hz_triple_t_then_half_then_full_second() {
    // Spec: render at t, t+0.5s, and t+1s. The half-second render must be
    // byte-identical to the t render (same wall-clock second). The
    // t+1s render must differ — and only in the seconds field of the
    // leading HH:MM:SS timestamp.
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("working on task".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_t = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_t = line_text(&line_t);

    clock.advance(Duration::from_millis(500));
    let line_half = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_half = line_text(&line_half);
    assert_eq!(
        text_t, text_half,
        "render at t+0.5s must be byte-identical to render at t"
    );

    clock.advance(Duration::from_millis(500));
    let line_t_plus_1 = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_t_plus_1 = line_text(&line_t_plus_1);
    assert_ne!(
        text_t, text_t_plus_1,
        "render at t+1s must differ from render at t"
    );

    // Only the leading HH:MM:SS timestamp's seconds field differs:
    // hours and minutes are stable, and everything after the timestamp
    // (spinner+body) is held constant by passing the same spinner_tick.
    let (hms_t, rest_t) = text_t.split_once(' ').expect("timestamp prefix");
    let (hms_1, rest_1) = text_t_plus_1.split_once(' ').expect("timestamp prefix");
    assert_eq!(rest_t, rest_1, "post-timestamp text must be unchanged");
    assert_eq!(
        &hms_t[..6],
        &hms_1[..6],
        "HH:MM portion must be unchanged across a 1s advance from {hms_t} to {hms_1}"
    );
    assert_ne!(
        &hms_t[6..],
        &hms_1[6..],
        ":SS portion must change across a 1s advance"
    );
}

#[test]
fn running_message_spinner_advances_per_frame() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("working".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_0 = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let line_1 = format_running_transcript_leaf(marker, &clock, 1, &fetcher);

    let text_0 = line_text(&line_0);
    let text_1 = line_text(&line_1);

    assert_ne!(text_0, text_1, "spinner should advance between frames");
    assert!(text_0.contains(SPINNER[0]));
    assert!(text_1.contains(SPINNER[1]));
}

#[test]
fn running_message_uses_live_summary() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("Processing files".to_string());
    let marker = TranscriptLeafMarker::new();

    let line = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text = line_text(&line);

    assert!(text.contains("Processing files"));
}

#[test]
fn running_message_body_gradient_moves_between_50ms_frames() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("gradient".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_0 = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let line_1 = format_running_transcript_leaf(marker, &clock, 1, &fetcher);
    let body_colors_0: Vec<_> = line_0.spans[2..].iter().map(|span| span.style.fg).collect();
    let body_colors_1: Vec<_> = line_1.spans[2..].iter().map(|span| span.style.fg).collect();

    assert_ne!(
        body_colors_0, body_colors_1,
        "50ms frame steps should move the live-summary body gradient"
    );
}

#[test]
fn running_message_fallback_when_empty() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = CachedSummaryFetcher::new("", "Brainstorm");
    let marker = TranscriptLeafMarker::new();

    let line = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text = line_text(&line);

    assert!(text.contains("Brainstorm"));
}

#[test]
fn extract_short_title_with_pipe() {
    let title = extract_short_title("Working on tests | Running cargo test suite");
    assert_eq!(title, "Working on tests");
}

#[test]
fn extract_short_title_without_pipe() {
    let title = extract_short_title("Simple title");
    assert_eq!(title, "Simple title");
}

#[test]
fn extract_short_title_trims_whitespace() {
    let title = extract_short_title("  Title with spaces  | body");
    assert_eq!(title, "Title with spaces");
}

#[test]
fn running_message_running_color() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("working".to_string());
    let marker = TranscriptLeafMarker::new();

    let line = format_running_transcript_leaf(marker, &clock, 0, &fetcher);

    let timestamp_span = &line.spans[0];
    let spinner_span = &line.spans[1];

    assert_eq!(timestamp_span.style.fg, Some(RUNNING_COLOR));
    assert_eq!(spinner_span.style.fg, Some(RUNNING_COLOR));
}

#[test]
fn same_indentation_as_historical() {
    let historical = format_historical_message(
        "14:30:25",
        "○",
        "message body",
        Color::DarkGray,
        HistoricalStyleHints::default(),
    );

    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("message body".to_string());
    let marker = TranscriptLeafMarker::new();
    let running = format_running_transcript_leaf(marker, &clock, 0, &fetcher);

    let hist_ts_len = historical.spans[0].content.chars().count();
    let run_ts_len = running.spans[0].content.chars().count();
    assert_eq!(hist_ts_len, run_ts_len, "timestamp field same width");
    assert_eq!(
        historical.spans[1].content.chars().count(),
        running.spans[1].content.chars().count(),
        "symbol field same width"
    );
    let historical_body: String = historical.spans[2..]
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    let running_body: String = running.spans[2..]
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(historical_body, running_body);
}

#[test]
fn frozen_clock_running_row_byte_identical() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("test".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_a = format_running_transcript_leaf(marker, &clock, 5, &fetcher);
    let line_b = format_running_transcript_leaf(marker, &clock, 5, &fetcher);

    let bytes_a: Vec<u8> = line_text(&line_a).bytes().collect();
    let bytes_b: Vec<u8> = line_text(&line_b).bytes().collect();

    assert_eq!(bytes_a, bytes_b, "byte-identical within same second");
}

#[test]
fn frozen_clock_difference_exactly_in_seconds_field() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let clock = TestClock::at(base);
    let fetcher = FixedFetcher("test".to_string());
    let marker = TranscriptLeafMarker::new();

    let line_before = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_before = line_text(&line_before);

    clock.advance(Duration::from_secs(1));

    let line_after = format_running_transcript_leaf(marker, &clock, 0, &fetcher);
    let text_after = line_text(&line_after);

    let before_chars: Vec<char> = text_before.chars().collect();
    let after_chars: Vec<char> = text_after.chars().collect();

    assert_eq!(before_chars.len(), after_chars.len());

    let mut diff_positions = Vec::new();
    for (i, (a, b)) in before_chars.iter().zip(after_chars.iter()).enumerate() {
        if a != b {
            diff_positions.push(i);
        }
    }

    assert!(
        !diff_positions.is_empty(),
        "should have at least one difference"
    );
    for pos in &diff_positions {
        assert!(
            *pos < 8,
            "difference at position {} should be in timestamp field (first 8 chars: HH:MM:SS)",
            pos
        );
    }
}

#[test]
fn gradient_spans_stage_shift_changes_at_least_one_pigment() {
    let a = gradient_spans("gradient", 0);
    let b = gradient_spans("gradient", 4);
    assert_eq!(a.len(), b.len());
    assert!(
        a.iter()
            .zip(b.iter())
            .any(|(left, right)| left.style.fg != right.style.fg),
        "stage shift should change at least one foreground color"
    );
}

#[test]
fn gradient_spans_count_scales_with_char_count() {
    let text = "abcdefghijklmnopqrstuvwxyz".repeat(12);
    let spans = gradient_spans(&text, 11);
    assert_eq!(spans.len(), text.chars().count());
}

#[test]
fn gradient_spans_non_ascii_still_allocates_per_char() {
    let text = "A語🙂B";
    let spans = gradient_spans(text, 7);
    assert_eq!(spans.len(), text.chars().count());
}
