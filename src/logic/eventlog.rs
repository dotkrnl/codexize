use crate::state::{Event, Message};
use anyhow::Context;
use serde::Deserialize;
#[derive(Debug, Default, Deserialize)]
struct EventsFile {
    #[serde(default)]
    events: Vec<Event>,
}
#[derive(Debug, Default, Deserialize)]
struct MessagesFile {
    #[serde(default)]
    messages: Vec<Message>,
}
pub fn parse_session_events_toml(text: &str) -> anyhow::Result<Vec<Event>> {
    let parsed: EventsFile = toml::from_str(text).context("failed to parse session events")?;
    Ok(parsed.events)
}
pub fn parse_messages_toml(text: &str) -> anyhow::Result<Vec<Message>> {
    let parsed: MessagesFile = toml::from_str(text).context("failed to parse session messages")?;
    Ok(parsed.messages)
}
