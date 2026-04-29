use crate::selection::VendorKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientUpdate {
    AgentMessageText(String),
    AgentThoughtText(String),
    SessionInfoUpdate { title: Option<String> },
    PromptTurnFinished,
    PromptTurnFailed { message: String },
    Unknown { kind: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpRuntimeEvent {
    Lifecycle(AcpLifecycleEvent),
    Text(AcpTextEvent),
    Completion(AcpCompletionEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpLifecycleEvent {
    SessionReady {
        session_id: String,
        vendor: VendorKind,
    },
    SessionTitleUpdated {
        title: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpTextEvent {
    pub text: String,
    pub interactive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpCompletionEvent {
    PromptTurnFinished,
    PromptTurnFailed { message: String },
}

pub fn translate_update(update: ClientUpdate, interactive: bool) -> Option<AcpRuntimeEvent> {
    match update {
        ClientUpdate::AgentMessageText(text) => {
            Some(AcpRuntimeEvent::Text(AcpTextEvent { text, interactive }))
        }
        ClientUpdate::AgentThoughtText(_) => None,
        ClientUpdate::SessionInfoUpdate { title } => title.map(|title| {
            AcpRuntimeEvent::Lifecycle(AcpLifecycleEvent::SessionTitleUpdated { title })
        }),
        ClientUpdate::PromptTurnFinished => Some(AcpRuntimeEvent::Completion(
            AcpCompletionEvent::PromptTurnFinished,
        )),
        ClientUpdate::PromptTurnFailed { message } => Some(AcpRuntimeEvent::Completion(
            AcpCompletionEvent::PromptTurnFailed { message },
        )),
        ClientUpdate::Unknown { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_chunks_become_text_events() {
        let event = translate_update(ClientUpdate::AgentMessageText("hello".to_string()), true)
            .expect("text event");

        assert_eq!(
            event,
            AcpRuntimeEvent::Text(AcpTextEvent {
                text: "hello".to_string(),
                interactive: true,
            })
        );
    }

    #[test]
    fn thought_chunks_are_ignored() {
        let event = translate_update(ClientUpdate::AgentThoughtText("internal".to_string()), true);

        assert!(event.is_none());
    }

    #[test]
    fn unknown_updates_are_ignored() {
        let event = translate_update(
            ClientUpdate::Unknown {
                kind: "future_update".to_string(),
            },
            false,
        );

        assert!(event.is_none());
    }
}
