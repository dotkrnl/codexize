use crate::selection::VendorKind;
use std::collections::VecDeque;

/// Logical-message boundary signal: `Continue` appends to the current live
/// block; `StartNewMessage` finalizes the live block first. The dispatcher
/// only emits `StartNewMessage` at explicit boundaries (session start,
/// prompt-turn reset, tool-call interleave, identity change).
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
    /// At most one `Start` (first non-terminal observation) and one `Finish`
    /// (first terminal observation) per `tool_call_id`. Runner stamps the
    /// time on receipt so consumers see arrival-ordered events.
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
    /// Runner-observable transition: at most once per id per kind. Runner
    /// stamps `Instant::now()` on consumption so the idle clock pauses at
    /// arrival, not at the App's next poll.
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
    let text_event = |text: String, thought: bool, boundary, identity| {
        AcpRuntimeEvent::Text(AcpTextEvent {
            text,
            interactive,
            thought,
            boundary,
            identity,
        })
    };
    match update {
        ClientUpdate::AgentMessageText {
            text,
            boundary,
            identity,
        } => Some(text_event(text, false, boundary, identity)),
        ClientUpdate::AgentThoughtText {
            text,
            boundary,
            identity,
        } => Some(text_event(text, true, boundary, identity)),
        ClientUpdate::ToolCallText {
            text,
            boundary,
            identity,
        } => Some(text_event(format!("{text}\n\n"), true, boundary, identity)),
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
#[path = "events_tests.rs"]
mod events_tests;
