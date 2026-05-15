//! Chat surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the session chat timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChatView {
    pub messages: Arc<[ChatMessage]>,
    pub scroll: ChatScrollWindow,
    /// True if the UI should auto-scroll to the bottom.
    pub follow_tail: bool,
}

/// One message in the chat timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub kind: ChatMessageKind,
    pub content: Arc<str>,
    /// Formatted timestamp (e.g. RFC3339).
    pub timestamp: Arc<str>,
}

/// UI-neutral message kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ChatMessageKind {
    Started,
    Brief,
    UserInput,
    AgentText,
    AgentThought,
    Summary,
    SummaryWarn,
    End,
}

/// Chat scroll state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ChatScrollWindow {
    pub offset: usize,
    pub visible_end: usize,
    pub show_above_indicator: bool,
    pub show_below_indicator: bool,
    pub above_count: usize,
    pub below_count: usize,
}
