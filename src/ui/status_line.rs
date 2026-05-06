use super::clock::{Clock, WallClock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::time::{Duration, Instant};
/// Severity drives both color and replacement priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    #[cfg_attr(not(test), allow(dead_code))]
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
#[path = "status_line_tests.rs"]
mod tests;
