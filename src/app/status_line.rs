use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Error,
}
#[derive(Debug)]
pub(crate) struct StatusMessage {
    pub(crate) text: String,
    pub(crate) severity: Severity,
    pub(crate) pushed_at: Instant,
    pub(crate) ttl: Duration,
}
impl StatusMessage {
    pub(crate) fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.pushed_at) >= self.ttl
    }
}
#[derive(Debug)]
pub struct StatusLine {
    current: Option<StatusMessage>,
}
impl StatusLine {
    pub fn new() -> Self {
        Self { current: None }
    }
    pub fn push(&mut self, message: String, severity: Severity, ttl: Duration) {
        let now = Instant::now();
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
    pub fn tick(&mut self) {
        let expired = self
            .current
            .as_ref()
            .is_some_and(|msg| msg.expired_at(Instant::now()));
        if expired {
            self.current = None;
        }
    }
    pub fn clear(&mut self) {
        self.current = None;
    }
    pub fn current_message(&self) -> Option<&StatusMessage> {
        self.current.as_ref()
    }
}
impl Default for StatusLine {
    fn default() -> Self {
        Self::new()
    }
}
