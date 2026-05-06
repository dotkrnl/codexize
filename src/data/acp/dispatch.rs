//! Translate ACP `session/update` payloads into `ClientUpdate`s.
//!
//! Text classification rule: emit `StartNewMessage` at explicit boundaries
//! (session start, prompt-turn reset, tool-call interleave, or stable identity
//! change); `Continue` otherwise.

use super::tool_call::{
    ToolCallDisplayState, ToolCallMap, format_invocation_line, format_result_line,
    is_terminal_status,
};
use super::{AcpTextBoundary, ClientUpdate, ToolCallActivityKind};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::Path;

#[derive(Debug, Clone, Default)]
struct StreamIdentity {
    last: Option<String>,
    restart: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct AcpBoundaryState {
    message: StreamIdentity,
    thought: StreamIdentity,
}

impl AcpBoundaryState {
    pub(super) fn new() -> Self {
        let mut s = Self::default();
        s.reset_for_prompt_turn();
        s
    }

    /// ACP servers may legally reuse message ids across turns, so the next
    /// turn must always restart at `StartNewMessage`.
    pub(super) fn reset_for_prompt_turn(&mut self) {
        self.message = StreamIdentity {
            last: None,
            restart: true,
        };
        self.thought = StreamIdentity {
            last: None,
            restart: true,
        };
    }
}

#[rustfmt::skip]
pub(super) fn dispatch_update(
    value: &Value, cwd: &Path, map: &mut ToolCallMap,
    boundary: &mut AcpBoundaryState, out: &mut VecDeque<ClientUpdate>,
) {
    if value.is_null() {
        out.push_back(ClientUpdate::Unknown { kind: "session/update".into() });
        return;
    }
    let kind = value.get("sessionUpdate").and_then(Value::as_str).unwrap_or("unknown");
    match kind {
        "agent_message_chunk" => push_text(value, &mut boundary.message, false, out),
        "agent_thought_chunk" => push_text(value, &mut boundary.thought, true, out),
        "session_info_update" => out.push_back(ClientUpdate::SessionInfoUpdate {
            title: value.get("title").and_then(Value::as_str).map(str::to_string),
        }),
        "tool_call" => {
            boundary.reset_for_prompt_turn();
            handle_tool_call(ToolCallDisplayState::from_value(value), cwd, map, out);
        }
        "tool_call_update" => {
            boundary.reset_for_prompt_turn();
            handle_tool_call_update(ToolCallDisplayState::from_value(value), map, out);
        }
        other => out.push_back(ClientUpdate::Unknown { kind: other.into() }),
    }
}

#[rustfmt::skip]
fn push_text(value: &Value, state: &mut StreamIdentity, thought: bool, out: &mut VecDeque<ClientUpdate>) {
    let text = value.pointer("/content/text").and_then(Value::as_str).unwrap_or_default().to_string();
    let identity = extract_identity(value);
    let boundary = classify_boundary(state, identity.as_deref());
    out.push_back(if thought {
        ClientUpdate::AgentThoughtText { text, boundary, identity }
    } else {
        ClientUpdate::AgentMessageText { text, boundary, identity }
    });
}

#[rustfmt::skip]
fn classify_boundary(state: &mut StreamIdentity, incoming: Option<&str>) -> AcpTextBoundary {
    let boundary = if state.restart {
        if let Some(id) = incoming { state.last = Some(id.to_string()); }
        AcpTextBoundary::StartNewMessage
    } else {
        match (incoming, state.last.as_deref()) {
            (Some(id), Some(last)) if last == id => AcpTextBoundary::Continue,
            (Some(id), _) => { state.last = Some(id.to_string()); AcpTextBoundary::StartNewMessage }
            (None, _) => AcpTextBoundary::Continue,
        }
    };
    state.restart = false;
    boundary
}

#[rustfmt::skip]
fn extract_identity(value: &Value) -> Option<String> {
    for ptr in ["/messageId", "/message_id", "/id", "/content/messageId", "/content/message_id", "/content/id"] {
        if let Some(id) = value.pointer(ptr).and_then(Value::as_str) && !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

#[rustfmt::skip]
fn handle_tool_call(state: ToolCallDisplayState, cwd: &Path, map: &mut ToolCallMap, out: &mut VecDeque<ClientUpdate>) {
    let terminal = state.status.as_deref().map(is_terminal_status).unwrap_or(false);
    let invocation = format_invocation_line(&state, cwd);
    let Some(id) = state.tool_call_id.clone() else {
        out.push_back(tool_call_text(invocation));
        if terminal { out.push_back(tool_call_text(format_result_line(&state))); }
        return;
    };
    map.insert(id.clone(), state.clone());
    out.push_back(tool_call_text(invocation));
    if terminal {
        out.push_back(tool_call_text(format_result_line(&state)));
        emit_activity_once(&id, true, map, out);
        map.evict(&id);
    } else {
        emit_activity_once(&id, false, map, out);
    }
}

#[rustfmt::skip]
fn handle_tool_call_update(payload: ToolCallDisplayState, map: &mut ToolCallMap, out: &mut VecDeque<ClientUpdate>) {
    let terminal = payload.status.as_deref().map(is_terminal_status).unwrap_or(false);
    let active = payload.status.as_deref().is_some_and(|s| matches!(s, "pending" | "in_progress"));
    let Some(id) = payload.tool_call_id.clone() else {
        if terminal { out.push_back(tool_call_text(format_result_line(&payload))); }
        return;
    };
    if let Some(state) = map.merge(&id, &payload) {
        if terminal {
            out.push_back(tool_call_text(format_result_line(state)));
            emit_activity_once(&id, true, map, out);
            map.evict(&id);
        } else if active {
            emit_activity_once(&id, false, map, out);
        }
    } else if terminal && !map.was_emitted(&id, true) {
        out.push_back(tool_call_text(format_result_line(&payload)));
        out.push_back(activity(&id, ToolCallActivityKind::Finish));
        map.mark_emitted(&id, true);
    }
}

#[rustfmt::skip]
fn emit_activity_once(id: &str, terminal: bool, map: &mut ToolCallMap, out: &mut VecDeque<ClientUpdate>) {
    if map.was_emitted(id, terminal) { return; }
    let kind = if terminal { ToolCallActivityKind::Finish } else { ToolCallActivityKind::Start };
    out.push_back(activity(id, kind));
    map.mark_emitted(id, terminal);
}

fn activity(id: &str, kind: ToolCallActivityKind) -> ClientUpdate {
    ClientUpdate::ToolCallActivity {
        tool_call_id: id.to_string(),
        kind,
    }
}

fn tool_call_text(text: String) -> ClientUpdate {
    ClientUpdate::ToolCallText {
        text,
        boundary: AcpTextBoundary::StartNewMessage,
        identity: None,
    }
}
