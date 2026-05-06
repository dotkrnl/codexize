use std::collections::VecDeque;

/// `Continue` appends to the current live block; `StartNewMessage` finalizes first.
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
    /// At most one Start/Finish per `tool_call_id`.
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
    SessionTitleUpdated { title: String },
    Text(AcpTextEvent),
    PromptTurnFinished,
    PromptTurnFailed { message: String },
    ToolCallActivity { tool_call_id: String, kind: ToolCallActivityKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpTextEvent {
    pub text: String,
    pub interactive: bool,
    pub thought: bool,
    pub boundary: AcpTextBoundary,
    pub identity: Option<String>,
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
            self.flush_ready();
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
        self.ready
            .pop_front()
            .or_else(|| (!self.buffer.is_empty()).then(|| std::mem::take(&mut self.buffer)))
    }

    fn flush_ready(&mut self) {
        while let Some(at) = self.buffer.find("\n\n") {
            let block = self.buffer[..at].to_string();
            self.buffer = self.buffer[at + 2..].to_string();
            if !block.is_empty() {
                self.ready.push_back(block);
            }
        }
        while self.buffer.chars().count() >= self.max_chars {
            let at = self
                .buffer
                .char_indices()
                .nth(self.max_chars)
                .map(|(i, _)| i)
                .unwrap_or(self.buffer.len());
            let block = self.buffer[..at].to_string();
            self.buffer = self.buffer[at..].to_string();
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
    let text = |text: String, thought, boundary, identity| {
        AcpRuntimeEvent::Text(AcpTextEvent {
            text,
            interactive,
            thought,
            boundary,
            identity,
        })
    };
    Some(match update {
        ClientUpdate::AgentMessageText {
            text: t,
            boundary,
            identity,
        } => text(t, false, boundary, identity),
        ClientUpdate::AgentThoughtText {
            text: t,
            boundary,
            identity,
        } => text(t, true, boundary, identity),
        ClientUpdate::ToolCallText {
            text: t,
            boundary,
            identity,
        } => text(format!("{t}\n\n"), true, boundary, identity),
        ClientUpdate::ToolCallActivity { tool_call_id, kind } => {
            AcpRuntimeEvent::ToolCallActivity { tool_call_id, kind }
        }
        ClientUpdate::SessionInfoUpdate { title } => {
            return title.map(|title| AcpRuntimeEvent::SessionTitleUpdated { title });
        }
        ClientUpdate::PromptTurnFinished => AcpRuntimeEvent::PromptTurnFinished,
        ClientUpdate::PromptTurnFailed { message } => AcpRuntimeEvent::PromptTurnFailed { message },
        ClientUpdate::Unknown { .. } => return None,
    })
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;
