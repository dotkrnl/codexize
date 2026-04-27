use std::time::Instant;

#[cfg(test)]
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, SystemTime},
};

/// Clock seam that abstracts wall-clock access for testability.
///
/// The 1 Hz timestamp truncation is a property of this layer: renders
/// within the same wall-clock second produce byte-identical timestamp
/// strings because the formatter never sees sub-second precision.
pub trait Clock: Clone + 'static {
    /// Monotonic instant for TTL calculations and relative timing.
    fn now(&self) -> Instant;

    /// Wall-clock timestamp truncated to whole seconds.
    ///
    /// Two calls within the same wall-clock second return byte-identical
    /// strings; calls across a second boundary differ.
    fn timestamp_string(&self) -> String;
}

/// Production clock backed by the system wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct WallClock;

impl WallClock {
    pub fn new() -> Self {
        Self
    }
}

impl Clock for WallClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn timestamp_string(&self) -> String {
        chrono::Local::now().format("%H:%M:%S").to_string()
    }
}

/// Test clock with independently controllable instant and system time.
///
/// `advance` moves both timelines forward so TTL expiry and timestamp
/// string changes can be tested without sleeping.
#[cfg(test)]
#[derive(Debug)]
struct TestClockState {
    instant: Instant,
    system: SystemTime,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct TestClock {
    state: Rc<RefCell<TestClockState>>,
}

#[cfg(test)]
impl TestClock {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(TestClockState {
                instant: Instant::now(),
                system: SystemTime::now(),
            })),
        }
    }

    pub fn at(system: SystemTime) -> Self {
        Self {
            state: Rc::new(RefCell::new(TestClockState {
                instant: Instant::now(),
                system,
            })),
        }
    }

    pub fn advance(&self, duration: Duration) {
        let mut state = self.state.borrow_mut();
        state.instant += duration;
        state.system += duration;
    }
}

#[cfg(test)]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.state.borrow().instant
    }

    fn timestamp_string(&self) -> String {
        let dt = chrono::DateTime::<chrono::Local>::from(self.state.borrow().system);
        dt.format("%H:%M:%S").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_timestamp_has_no_subseconds() {
        let ts = WallClock::new().timestamp_string();
        // HH:MM:SS has exactly 8 characters.
        assert_eq!(ts.len(), 8, "timestamp should be HH:MM:SS, got: {ts}");
        assert!(ts.contains(':'));
    }

    #[test]
    fn test_clock_timestamp_stable_within_second() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let a = clock.timestamp_string();
        let b = clock.timestamp_string();
        assert_eq!(a, b, "same-second reads must be byte-identical");
    }

    #[test]
    fn test_clock_timestamp_changes_across_second_boundary() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let before = clock.timestamp_string();
        clock.advance(Duration::from_secs(1));
        let after = clock.timestamp_string();
        assert_ne!(
            before, after,
            "timestamp must differ after crossing a second boundary"
        );
    }

    #[test]
    fn test_clock_timestamp_stable_at_half_second_offset() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::at(base);
        let at_start = clock.timestamp_string();
        clock.advance(Duration::from_millis(500));
        let at_half = clock.timestamp_string();
        assert_eq!(
            at_start, at_half,
            "half-second offset inside the same second must not change timestamp"
        );
    }
}
