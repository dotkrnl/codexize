use codexize::logic::eventlog::{parse_messages_toml, parse_session_events_toml};
use codexize::logic::pipeline::Phase;
use codexize::state::MessageKind;

#[test]
fn parse_session_events_toml_reads_pure_event_slices() {
    let events = parse_session_events_toml(
        r#"
[[events]]
timestamp = "2026-05-04T12:00:00Z"
phase = "PlanningRunning"
message = "planning started"
"#,
    )
    .expect("events should parse");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].phase, Phase::PlanningRunning);
    assert_eq!(events[0].message, "planning started");
}

#[test]
fn parse_messages_toml_reads_message_history_without_fs_access() {
    let messages = parse_messages_toml(
        r#"
[[messages]]
ts = "2026-05-04T12:00:00Z"
run_id = 9
kind = "Started"
text = "launch"

[messages.sender]
System = {}
"#,
    )
    .expect("messages should parse");

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Started);
    assert_eq!(messages[0].text, "launch");
}
