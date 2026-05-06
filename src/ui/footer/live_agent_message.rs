pub use super::live_agent_message_view_model::extract_short_title;
use super::live_agent_message_view_model::{capitalize_first, gradient_spans};
use crate::ui::clock::Clock;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Running-state color per spec.
const RUNNING_COLOR: Color = Color::Blue;
/// Style hints for historical message rendering.
#[derive(Clone, Copy, Debug, Default)]
pub struct HistoricalStyleHints {
    pub is_summary: bool,
    pub is_warning: bool,
    pub is_error: bool,
    pub is_dim: bool,
}
/// Format a historical (completed) agent message line.
///
/// This is the standard format for transcript-style leaf rows that have
/// finished running. The shape is:
///
/// `HH:MM:SS ○ body text`
///
/// Where the timestamp and symbol colors vary by message type.
pub fn format_historical_message(
    timestamp: &str,
    symbol: &str,
    body: &str,
    symbol_color: Color,
    hints: HistoricalStyleHints,
) -> Line<'static> {
    let body_style = if hints.is_error {
        Style::default().fg(Color::Red)
    } else if hints.is_warning {
        Style::default().fg(Color::Yellow)
    } else if hints.is_summary {
        Style::default().fg(Color::Green)
    } else if hints.is_dim {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let capitalized_body = capitalize_first(body);
    Line::from(vec![
        Span::styled(format!("{} ", timestamp), Style::default().fg(symbol_color)),
        Span::styled(format!("{} ", symbol), Style::default().fg(symbol_color)),
        Span::styled(capitalized_body, body_style),
    ])
}
/// Marker type for running transcript leaves.
///
/// This type exists solely to enforce at compile-time that only transcript
/// leaves (not container rows) can use `format_running_transcript_leaf`.
/// Container rows (stages, tasks, artifacts) keep their tree-node shape
/// with spinner + state label and should use a different rendering path.
// TranscriptLeafMarker and the transcript leaf formatters will be used by
// the split renderer once transcript tails move out of the tree body.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct TranscriptLeafMarker(());
impl TranscriptLeafMarker {
    /// Create a new marker. The caller asserts this is truly a transcript leaf.
    pub fn new() -> Self {
        Self(())
    }
}
impl Default for TranscriptLeafMarker {
    fn default() -> Self {
        Self::new()
    }
}
/// Live-summary fetcher trait.
///
/// This seam allows testing without filesystem access. The production
/// implementation wraps the existing mtime-cached reader.
#[allow(dead_code)]
pub trait LiveSummaryFetcher {
    /// Fetch the current short title from the live summary file.
    ///
    /// Returns the cached short title or the fallback phase label when:
    /// - No live summary file exists
    /// - The file is being rewritten (partial read)
    fn fetch(&self) -> String;
}
/// Render-time fetcher that wraps App's pre-cached live-summary text.
///
/// The runtime tick (`TerminalRuntime::drain_app_data_events` together with
/// `App::poll_live_summary_fallback` / `App::read_live_summary_pipeline`)
/// already performs mtime-based file reading with fallback to the last cached value
/// on partial reads. This struct borrows that cached result at render time and
/// extracts the short title, avoiding filesystem I/O on the render path.
#[allow(dead_code)]
pub struct CachedSummaryFetcher<'a> {
    cached_text: &'a str,
    phase_fallback: &'a str,
}
impl<'a> CachedSummaryFetcher<'a> {
    pub fn new(cached_text: &'a str, phase_fallback: &'a str) -> Self {
        Self {
            cached_text,
            phase_fallback,
        }
    }
}
impl LiveSummaryFetcher for CachedSummaryFetcher<'_> {
    fn fetch(&self) -> String {
        if self.cached_text.is_empty() {
            self.phase_fallback.to_string()
        } else {
            extract_short_title(self.cached_text)
        }
    }
}
/// Test fetcher that returns a fixed value.
#[cfg(test)]
pub struct FixedFetcher(pub String);
#[cfg(test)]
impl LiveSummaryFetcher for FixedFetcher {
    fn fetch(&self) -> String {
        self.0.clone()
    }
}
/// Format a running transcript-style leaf row.
///
/// This produces a line identical in shape to historical messages:
///
/// `HH:MM:SS ⠋ live summary title`
///
/// The timestamp is taken from the Clock (1 Hz truncated), and the spinner
/// advances per frame independently.
///
/// # Type Safety
///
/// This function takes a `TranscriptLeafMarker` to enforce at compile-time
/// that only transcript-style leaf rows use this format. Container rows
/// (stages, tasks, artifacts) must use a different rendering path that
/// preserves their tree-node structure.
///
/// # Arguments
///
/// * `_marker` - Proof that this is a transcript leaf row.
/// * `clock` - Clock providing 1 Hz truncated timestamps.
/// * `spinner_tick` - Frame counter for spinner animation.
/// * `fetcher` - Live summary text fetcher.
#[allow(dead_code)]
pub fn format_running_transcript_leaf<C: Clock>(
    _marker: TranscriptLeafMarker,
    clock: &C,
    spinner_tick: usize,
    fetcher: &impl LiveSummaryFetcher,
) -> Line<'static> {
    let timestamp = clock.timestamp_string();
    let spinner = SPINNER[spinner_tick % SPINNER.len()];
    let body = capitalize_first(&fetcher.fetch());
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
    Line::from(spans)
}
/// Format a stalled transcript-style leaf row.
///
/// The row keeps the same spinner-shaped marker as active rows, but freezes it
/// and labels the state so lack of transcript activity is visible without
/// implying the run completed.
#[allow(dead_code)]
pub fn format_stalled_transcript_leaf<C: Clock>(
    _marker: TranscriptLeafMarker,
    clock: &C,
    fetcher: &impl LiveSummaryFetcher,
) -> Line<'static> {
    let timestamp = clock.timestamp_string();
    let body = capitalize_first(&fetcher.fetch());
    Line::from(vec![
        Span::styled(
            format!("{} ", timestamp),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("{} ", SPINNER[0]),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("stalled", Style::default().fg(Color::Yellow)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(body, Style::default().fg(Color::DarkGray)),
    ])
}
#[cfg(test)]
#[path = "live_agent_message_tests.rs"]
mod tests;
