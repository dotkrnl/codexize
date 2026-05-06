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
