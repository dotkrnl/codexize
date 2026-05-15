use super::*;

fn line_at(clock: crate::ui::clock::TestClock) -> StatusLine<crate::ui::clock::TestClock> {
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
    let clock = crate::ui::clock::TestClock::new();
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
    let clock = crate::ui::clock::TestClock::new();
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
