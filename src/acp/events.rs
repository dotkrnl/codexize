use crate::selection::VendorKind;
use std::collections::VecDeque;

/// Logical-message boundary signal carried alongside ACP text payloads.
///
/// `Continue` means the runner may append the chunk to the current live block
/// on the matching stream. `StartNewMessage` means the runner must finalize
/// any current live block before pushing this chunk through its accumulator.
///
/// The dispatcher only emits `StartNewMessage` at explicit boundaries —
/// session start, prompt-turn reset, tool-call interleave, or a stable
/// identity change. No-identity mid-stream chunks default to `Continue` so
/// one streamed response is not over-split into one persisted message per
/// chunk. Intra-message paragraph splits are still handled downstream by
/// `AcpTextAccumulator`'s blank-line splitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpTextBoundary {
    Continue,
    StartNewMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientUpdate {
    AgentMessageText {
        text: String,
        boundary: AcpTextBoundary,
        identity: Option<String>,
    },
    AgentThoughtText {
        text: String,
        boundary: AcpTextBoundary,
        identity: Option<String>,
    },
    ToolCallText {
        text: String,
        boundary: AcpTextBoundary,
        identity: Option<String>,
    },
    /// Lifecycle transition for a single `tool_call_id`. The dispatcher
    /// emits at most one `Start` (the first time the id is observed in a
    /// non-terminal status) and at most one `Finish` (the first time it
    /// is observed in a terminal status). The runner timestamps each
    /// transition when it receives the `AcpRuntimeEvent` that this
    /// update produces, so consumers see arrival-ordered events even for
    /// short tool calls that start and finish between poll cycles.
    ToolCallActivity {
        tool_call_id: String,
        kind: ToolCallActivityKind,
    },
    SessionInfoUpdate {
        title: Option<String>,
    },
    PromptTurnFinished,
    PromptTurnFailed {
        message: String,
    },
    Unknown {
        kind: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallActivityKind {
    Start,
    Finish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpRuntimeEvent {
    Lifecycle(AcpLifecycleEvent),
    Text(AcpTextEvent),
    Completion(AcpCompletionEvent),
    /// Runner-observable lifecycle transition for a single `tool_call_id`.
    /// Emitted at most once per id per kind. Carries no timestamp itself;
    /// the runner stamps `Instant::now()` as it consumes the event so the
    /// idle-adjusted clock pauses at the moment the runner saw the
    /// transition rather than at the App's next poll.
    ToolCallActivity {
        tool_call_id: String,
        kind: ToolCallActivityKind,
    },
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
    pub thought: bool,
    pub boundary: AcpTextBoundary,
    pub identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpCompletionEvent {
    PromptTurnFinished,
    PromptTurnFailed { message: String },
}

#[derive(Debug, Clone)]
pub struct AcpTextAccumulator {
    buffer: String,
    max_chars: usize,
    ready: VecDeque<String>,
}

impl AcpTextAccumulator {
    pub const DEFAULT_MAX_CHARS: usize = 8192;

    pub fn new() -> Self {
        Self::with_max_chars(Self::DEFAULT_MAX_CHARS)
    }

    pub fn with_max_chars(max_chars: usize) -> Self {
        Self {
            buffer: String::new(),
            max_chars: max_chars.max(1),
            ready: VecDeque::new(),
        }
    }

    pub fn push(&mut self, chunk: &str) -> Option<String> {
        if !chunk.is_empty() {
            self.buffer.push_str(chunk);
            self.flush_ready_blocks();
        }
        self.ready.pop_front()
    }

    pub fn current_text(&self) -> Option<&str> {
        (!self.buffer.is_empty()).then_some(self.buffer.as_str())
    }

    pub fn next_ready(&mut self) -> Option<String> {
        self.ready.pop_front()
    }

    pub fn finish_prompt_turn(&mut self) -> Option<String> {
        if let Some(ready) = self.ready.pop_front() {
            return Some(ready);
        }
        (!self.buffer.is_empty()).then(|| std::mem::take(&mut self.buffer))
    }

    fn flush_ready_blocks(&mut self) {
        while let Some(split_at) = self.buffer.find("\n\n") {
            let block = self.buffer[..split_at].to_string();
            self.buffer = self.buffer[split_at + 2..].to_string();
            if !block.is_empty() {
                self.ready.push_back(block);
            }
        }
        while self.buffer.chars().count() >= self.max_chars {
            let split_at = self
                .buffer
                .char_indices()
                .nth(self.max_chars)
                .map(|(idx, _)| idx)
                .unwrap_or(self.buffer.len());
            let block = self.buffer[..split_at].to_string();
            self.buffer = self.buffer[split_at..].to_string();
            self.ready.push_back(block);
        }
    }
}

impl Default for AcpTextAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

pub fn translate_update(update: ClientUpdate, interactive: bool) -> Option<AcpRuntimeEvent> {
    match update {
        ClientUpdate::AgentMessageText {
            text,
            boundary,
            identity,
        } => Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text,
            interactive,
            thought: false,
            boundary,
            identity,
        })),
        ClientUpdate::AgentThoughtText {
            text,
            boundary,
            identity,
        } => Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text,
            interactive,
            thought: true,
            boundary,
            identity,
        })),
        ClientUpdate::ToolCallText {
            text,
            boundary,
            identity,
        } => Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text: format!("{text}\n\n"),
            interactive,
            thought: true,
            boundary,
            identity,
        })),
        ClientUpdate::ToolCallActivity { tool_call_id, kind } => {
            Some(AcpRuntimeEvent::ToolCallActivity { tool_call_id, kind })
        }
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
        let event = translate_update(
            ClientUpdate::AgentMessageText {
                text: "hello".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            },
            true,
        )
        .expect("text event");

        assert_eq!(
            event,
            AcpRuntimeEvent::Text(AcpTextEvent {
                text: "hello".to_string(),
                interactive: true,
                thought: false,
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            })
        );
    }

    #[test]
    fn message_chunks_preserve_continue_boundary() {
        let event = translate_update(
            ClientUpdate::AgentMessageText {
                text: " more".to_string(),
                boundary: AcpTextBoundary::Continue,
                identity: None,
            },
            true,
        )
        .expect("text event");

        assert_eq!(
            event,
            AcpRuntimeEvent::Text(AcpTextEvent {
                text: " more".to_string(),
                interactive: true,
                thought: false,
                boundary: AcpTextBoundary::Continue,
                identity: None,
            })
        );
    }

    #[test]
    fn thought_chunks_are_ignored() {
        let event = translate_update(
            ClientUpdate::AgentThoughtText {
                text: "internal".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            },
            true,
        );

        assert_eq!(
            event,
            Some(AcpRuntimeEvent::Text(AcpTextEvent {
                text: "internal".to_string(),
                interactive: true,
                thought: true,
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }))
        );
    }

    #[test]
    fn tool_call_text_becomes_thought_event_with_paragraph_break() {
        let event = translate_update(
            ClientUpdate::ToolCallText {
                text: "tool: read(Cargo.toml)".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            },
            false,
        );

        assert_eq!(
            event,
            Some(AcpRuntimeEvent::Text(AcpTextEvent {
                text: "tool: read(Cargo.toml)\n\n".to_string(),
                interactive: false,
                thought: true,
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }))
        );
    }

    #[test]
    fn tool_call_text_translates_unchanged_for_result_blocks() {
        let event = translate_update(
            ClientUpdate::ToolCallText {
                text: "result: completed, exit 0, output: ok".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            },
            true,
        );

        assert_eq!(
            event,
            Some(AcpRuntimeEvent::Text(AcpTextEvent {
                text: "result: completed, exit 0, output: ok\n\n".to_string(),
                interactive: true,
                thought: true,
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }))
        );
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

    #[test]
    fn text_accumulator_flushes_bounded_prompt_turn_blocks() {
        let mut accumulator = AcpTextAccumulator::with_max_chars(10);

        assert!(accumulator.push("hello").is_none());
        assert_eq!(accumulator.push(" world"), Some("hello worl".to_string()));
        assert_eq!(
            accumulator.finish_prompt_turn(),
            Some("d".to_string()),
            "overflow text remains in the current prompt-turn block"
        );
    }

    #[test]
    fn text_accumulator_ignores_empty_chunks() {
        let mut accumulator = AcpTextAccumulator::with_max_chars(8);

        assert!(accumulator.push("").is_none());
        assert!(accumulator.finish_prompt_turn().is_none());
    }

    #[test]
    fn text_accumulator_keeps_partial_text_live_and_splits_paragraphs() {
        let mut accumulator = AcpTextAccumulator::with_max_chars(80);

        assert!(accumulator.push("thinking").is_none());
        assert_eq!(accumulator.current_text(), Some("thinking"));

        assert_eq!(
            accumulator.push(" aloud\n\nnext thought"),
            Some("thinking aloud".to_string())
        );
        assert_eq!(accumulator.current_text(), Some("next thought"));
        assert_eq!(
            accumulator.finish_prompt_turn(),
            Some("next thought".to_string())
        );
    }

    #[test]
    fn finish_prompt_turn_drains_queued_ready_blocks_before_live_buffer() {
        // Spec: a prompt turn must not swallow a queued ready block when a
        // live block also exists. `finish_prompt_turn` returns one block per
        // call so the caller can loop and persist each finalized block before
        // the live remainder. Drain order is ready blocks first (in arrival
        // order), then the live buffer.
        let mut accumulator = AcpTextAccumulator::with_max_chars(80);

        assert_eq!(
            accumulator.push("first paragraph\n\nsecond paragraph\n\nlive"),
            Some("first paragraph".to_string())
        );
        // Caller has only consumed one ready block so far; another ready
        // block plus the live buffer remain. finish_prompt_turn must surface
        // both rather than collapsing them.
        assert_eq!(
            accumulator.finish_prompt_turn(),
            Some("second paragraph".to_string())
        );
        assert_eq!(accumulator.finish_prompt_turn(), Some("live".to_string()));
        assert!(accumulator.finish_prompt_turn().is_none());
    }
}
