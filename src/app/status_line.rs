use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::clock::{Clock, WallClock};

/// Severity drives both color and replacement priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// A single transient status message with a TTL.
#[derive(Debug)]
struct StatusMessage {
    text: String,
    severity: Severity,
    pushed_at: Instant,
    ttl: Duration,
}

/// General-purpose transient status queue.
///
/// Owned by `App` and shared with non-render call sites via
/// `Rc<RefCell<StatusLine>>`. The TUI loop is single-threaded so no
/// locking primitives are required.
#[derive(Debug)]
pub struct StatusLine<C = WallClock>
where
    C: Clock,
{
    current: Option<StatusMessage>,
    clock: C,
}

impl StatusLine {
    pub fn new() -> Self {
        Self::with_clock(WallClock::new())
    }
}

impl Default for StatusLine {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> StatusLine<C>
where
    C: Clock,
{
    pub fn with_clock(clock: C) -> Self {
        Self {
            current: None,
            clock,
        }
    }

    /// Push a message. Severity priority is enforced here:
    ///
    /// - An incoming message replaces the current one only if its severity
    ///   is *greater than or equal to* the current message's severity.
    /// - Equal-severity uses most-recent-wins.
    /// - After TTL expiry, any severity may take the line.
    pub fn push(&mut self, message: String, severity: Severity, ttl: Duration) {
        let now = self.clock.now();
        let should_replace = match &self.current {
            None => true,
            Some(msg) if msg.expired_at(now) => true,
            Some(msg) => severity >= msg.severity,
        };

        if should_replace {
            self.current = Some(StatusMessage {
                text: message,
                severity,
                pushed_at: now,
                ttl,
            });
        }
    }

    /// Expire any message whose TTL has elapsed relative to `now`.
    pub fn tick(&mut self, now: Instant) {
        let expired = self.current.as_ref().is_some_and(|msg| msg.expired_at(now));
        if expired {
            self.current = None;
        }
    }

    /// Clear the current message so a later lower-severity push can take the line.
    pub fn clear(&mut self) {
        self.current = None;
    }

    /// Render the current message to 0 or 1 `Line`.
    ///
    /// The caller should `tick` first so that expired messages are
    /// cleared before rendering.
    pub fn render(&self) -> Option<Line<'static>> {
        let msg = self.current.as_ref()?;
        let color = match msg.severity {
            Severity::Info => Color::Gray,
            Severity::Warn => Color::Yellow,
            Severity::Error => Color::Red,
        };
        Some(Line::from(Span::styled(
            msg.text.clone(),
            Style::default().fg(color),
        )))
    }
}

impl StatusMessage {
    fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.pushed_at) >= self.ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_at(
        clock: super::super::clock::TestClock,
    ) -> StatusLine<super::super::clock::TestClock> {
        StatusLine::with_clock(clock)
    }

    #[test]
    fn info_replaced_by_higher_severity_error() {
        let mut line = StatusLine::new();
        line.push(
            "info msg".to_string(),
            Severity::Info,
            Duration::from_secs(10),
        );
        line.push(
            "error msg".to_string(),
            Severity::Error,
            Duration::from_secs(10),
        );
        let rendered = line.render().expect("should have a message");
        assert_eq!(rendered.to_string(), "error msg");
    }

    #[test]
    fn error_not_silently_overwritten_by_info() {
        let mut line = StatusLine::new();
        line.push(
            "error msg".to_string(),
            Severity::Error,
            Duration::from_secs(10),
        );
        line.push(
            "info msg".to_string(),
            Severity::Info,
            Duration::from_secs(10),
        );
        let rendered = line.render().expect("should have a message");
        assert_eq!(rendered.to_string(), "error msg");
    }

    #[test]
    fn equal_severity_uses_most_recent_wins() {
        let mut line = StatusLine::new();
        line.push("first".to_string(), Severity::Info, Duration::from_secs(10));
        line.push(
            "second".to_string(),
            Severity::Info,
            Duration::from_secs(10),
        );
        let rendered = line.render().expect("should have a message");
        assert_eq!(rendered.to_string(), "second");
    }

    #[test]
    fn lower_severity_can_take_line_after_ttl_expiry() {
        let clock = super::super::clock::TestClock::new();
        let now = clock.now();
        let mut line = line_at(clock.clone());
        line.push(
            "error msg".to_string(),
            Severity::Error,
            Duration::from_millis(100),
        );

        clock.advance(Duration::from_millis(150));
        let later = clock.now();
        line.tick(later);
        assert!(line.render().is_none(), "error should have expired");

        line.push(
            "info msg".to_string(),
            Severity::Info,
            Duration::from_secs(10),
        );
        let rendered = line
            .render()
            .expect("info should take the line after expiry");
        assert_eq!(rendered.to_string(), "info msg");
        assert!(later.duration_since(now) >= Duration::from_millis(150));
    }

    #[test]
    fn ttl_expiry_hides_message() {
        let clock = super::super::clock::TestClock::new();
        let mut line = line_at(clock.clone());
        line.push(
            "transient".to_string(),
            Severity::Info,
            Duration::from_millis(50),
        );
        assert!(line.render().is_some());

        clock.advance(Duration::from_millis(100));
        line.tick(clock.now());
        assert!(
            line.render().is_none(),
            "message should be hidden after TTL"
        );
    }

    #[test]
    fn explicit_clear_allows_lower_severity_replacement() {
        let mut line = StatusLine::new();
        line.push(
            "error msg".to_string(),
            Severity::Error,
            Duration::from_secs(10),
        );

        line.clear();
        line.push(
            "info msg".to_string(),
            Severity::Info,
            Duration::from_secs(10),
        );

        let rendered = line.render().expect("info should render after clear");
        assert_eq!(rendered.to_string(), "info msg");
    }

    #[test]
    fn empty_line_renders_nothing() {
        let line = StatusLine::new();
        assert!(line.render().is_none());
    }

    #[test]
    fn severity_colors() {
        let mut line = StatusLine::new();

        line.push("i".to_string(), Severity::Info, Duration::from_secs(10));
        let info_span = line.render().unwrap().spans[0].clone();
        assert_eq!(info_span.style.fg, Some(Color::Gray));

        line.push("w".to_string(), Severity::Warn, Duration::from_secs(10));
        let warn_span = line.render().unwrap().spans[0].clone();
        assert_eq!(warn_span.style.fg, Some(Color::Yellow));

        line.push("e".to_string(), Severity::Error, Duration::from_secs(10));
        let err_span = line.render().unwrap().spans[0].clone();
        assert_eq!(err_span.style.fg, Some(Color::Red));
    }
}
