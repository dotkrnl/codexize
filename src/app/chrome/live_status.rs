use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::super::{
    clock::Clock,
    footer::{LiveSummaryFetcher, gradient_spans},
};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const RUNNING_COLOR: Color = Color::Blue;

pub fn live_status_line<C: Clock>(
    clock: &C,
    spinner_tick: usize,
    fetcher: &impl LiveSummaryFetcher,
    width: u16,
) -> Line<'static> {
    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }

    let timestamp = clock.timestamp_string();
    let spinner = SPINNER[spinner_tick % SPINNER.len()];
    let body = fetcher.fetch();

    let mut spans = vec![
        Span::styled(
            format!("{} ", timestamp),
            Style::default().fg(RUNNING_COLOR),
        ),
        Span::styled(
            format!("{} ", spinner),
            Style::default()
                .fg(RUNNING_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    spans.extend(gradient_spans(&body, spinner_tick));

    let used_width = UnicodeWidthStr::width(timestamp.as_str())
        + 1
        + UnicodeWidthStr::width(spinner)
        + 1
        + UnicodeWidthStr::width(body.as_str());
    let target = width as usize;
    if target > used_width {
        spans.push(Span::styled(
            " ".repeat(target - used_width),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::clock::TestClock;
    use std::time::{Duration, SystemTime};

    struct StubFetcher(String);

    impl LiveSummaryFetcher for StubFetcher {
        fn fetch(&self) -> String {
            self.0.clone()
        }
    }

    fn line_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn live_status_line_keeps_timestamp_spinner_shape() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let fetcher = StubFetcher("Awaiting idea".to_string());

        let line = live_status_line(&clock, 0, &fetcher, 80);
        let text = line_text(&line);

        assert!(text.contains("⠋"));
        assert!(text.contains("Awaiting idea"));
        assert_eq!(line.spans[0].style.fg, Some(RUNNING_COLOR));
        assert_eq!(line.spans[1].style.fg, Some(RUNNING_COLOR));
    }

    #[test]
    fn live_status_line_phase_changes_spinner_and_gradient() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let fetcher = StubFetcher("Awaiting".to_string());

        let line_a = live_status_line(&clock, 0, &fetcher, 80);
        let line_b = live_status_line(&clock, 1, &fetcher, 80);

        assert!(line_text(&line_a).contains(SPINNER[0]));
        assert!(line_text(&line_b).contains(SPINNER[1]));
        assert!(
            line_a
                .spans
                .iter()
                .zip(line_b.spans.iter())
                .any(|(left, right)| left.style.fg != right.style.fg),
            "phase shift should change spinner and/or body colors"
        );
    }

    #[test]
    fn live_status_line_pads_to_target_width() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let fetcher = StubFetcher("Done".to_string());
        let width = 60;

        let line = live_status_line(&clock, 3, &fetcher, width);
        let text = line_text(&line);

        assert_eq!(UnicodeWidthStr::width(text.as_str()), width as usize);
        let last = line.spans.last().expect("padding span");
        assert_eq!(last.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn live_status_line_padding_uses_display_width_for_non_ascii_body() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let fetcher = StubFetcher("語🙂".to_string());
        let width = 64;

        let line = live_status_line(&clock, 5, &fetcher, width);
        let text = line_text(&line);

        assert_eq!(UnicodeWidthStr::width(text.as_str()), width as usize);
    }
}
