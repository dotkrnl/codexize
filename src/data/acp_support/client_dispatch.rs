//! Translate ACP `session/update` payloads into `ClientUpdate`s.
//!
//! Text classification rule: emit `StartNewMessage` at explicit boundaries
//! (session start, prompt-turn reset, tool-call interleave, or stable identity
//! change); `Continue` otherwise. Real ACP servers emit `agent_message_chunk`
//! events without stable ids, so no-identity mid-stream chunks default to
//! `Continue` rather than over-splitting one streamed response.

use crate::data::acp::{AcpTextBoundary, ClientUpdate, ToolCallActivityKind};
use crate::data::acp_support::tool_call::{
    ToolCallDisplayState, ToolCallMap, format_invocation_line, format_result_line,
    is_terminal_status,
};
use serde_json::Value;
use std::{collections::VecDeque, path::Path};

#[derive(Debug, Clone)]
struct StreamIdentity {
    last_identity: Option<String>,
    restart_pending: bool,
}

impl StreamIdentity {
    fn fresh() -> Self {
        Self {
            last_identity: None,
            restart_pending: true,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct AcpBoundaryState {
    agent_message: StreamIdentity,
    agent_thought: StreamIdentity,
}

impl AcpBoundaryState {
    pub(super) fn new() -> Self {
        Self {
            agent_message: StreamIdentity::fresh(),
            agent_thought: StreamIdentity::fresh(),
        }
    }

    /// Reset both streams at a prompt-turn boundary. ACP servers may legally
    /// reuse message ids across turns, so the next turn must always restart
    /// at `StartNewMessage`.
    pub(super) fn reset_for_prompt_turn(&mut self) {
        self.agent_message = StreamIdentity::fresh();
        self.agent_thought = StreamIdentity::fresh();
    }

    fn reset_for_tool_call(&mut self) {
        self.reset_for_prompt_turn();
    }
}

/// Translate one ACP `session/update` payload into zero or more visible
/// `ClientUpdate`s. A single `tool_call` payload may yield two updates
/// (invocation + result) when its status is already terminal; non-terminal
/// `tool_call_update`s with prior state are silently merged.
pub(super) fn dispatch_update(
    value: &Value,
    cwd: &Path,
    map: &mut ToolCallMap,
    boundary_state: &mut AcpBoundaryState,
    out: &mut VecDeque<ClientUpdate>,
) {
    if value.is_null() {
        out.push_back(ClientUpdate::Unknown {
            kind: "session/update".to_string(),
        });
        return;
    }

    let kind = value
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match kind {
        "agent_message_chunk" => push_text_chunk(
            value,
            &mut boundary_state.agent_message,
            |text, boundary, identity| ClientUpdate::AgentMessageText {
                text,
                boundary,
                identity,
            },
            out,
        ),
        "agent_thought_chunk" => push_text_chunk(
            value,
            &mut boundary_state.agent_thought,
            |text, boundary, identity| ClientUpdate::AgentThoughtText {
                text,
                boundary,
                identity,
            },
            out,
        ),
        "session_info_update" => {
            out.push_back(ClientUpdate::SessionInfoUpdate {
                title: value
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        "tool_call" => {
            // A tool-call invocation interleaves the visible stream and acts
            // as a hard boundary for both agent and thought streams.
            boundary_state.reset_for_tool_call();
            handle_tool_call(ToolCallDisplayState::from_value(value), cwd, map, out);
        }
        "tool_call_update" => {
            boundary_state.reset_for_tool_call();
            handle_tool_call_update(ToolCallDisplayState::from_value(value), map, out);
        }
        other => out.push_back(ClientUpdate::Unknown {
            kind: other.to_string(),
        }),
    }
}

fn push_text_chunk(
    value: &Value,
    state: &mut StreamIdentity,
    build: impl FnOnce(String, AcpTextBoundary, Option<String>) -> ClientUpdate,
    out: &mut VecDeque<ClientUpdate>,
) {
    let text = value
        .pointer("/content/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let identity = extract_message_identity(value);
    let boundary = boundary_for_text_chunk(state, identity.as_deref());
    out.push_back(build(text, boundary, identity));
}

/// Classify a text chunk relative to per-stream state. `restart_pending` is
/// the only source of `StartNewMessage` for no-identity chunks: it is set at
/// session start and at every explicit boundary, and cleared once a chunk
/// has been classified.
fn boundary_for_text_chunk(state: &mut StreamIdentity, incoming: Option<&str>) -> AcpTextBoundary {
    let boundary = if state.restart_pending {
        if let Some(id) = incoming {
            state.last_identity = Some(id.to_string());
        }
        AcpTextBoundary::StartNewMessage
    } else {
        match (incoming, state.last_identity.as_deref()) {
            (Some(id), Some(last)) if last == id => AcpTextBoundary::Continue,
            (Some(id), _) => {
                state.last_identity = Some(id.to_string());
                AcpTextBoundary::StartNewMessage
            }
            (None, _) => AcpTextBoundary::Continue,
        }
    };
    state.restart_pending = false;
    boundary
}

/// The ACP spec does not mandate a single field name for a stable message id,
/// so this checks the most plausible locations.
fn extract_message_identity(value: &Value) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "/messageId",
        "/message_id",
        "/id",
        "/content/messageId",
        "/content/message_id",
        "/content/id",
    ];
    CANDIDATES.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    })
}

fn handle_tool_call(
    state: ToolCallDisplayState,
    cwd: &Path,
    map: &mut ToolCallMap,
    out: &mut VecDeque<ClientUpdate>,
) {
    let terminal = state
        .status
        .as_deref()
        .map(is_terminal_status)
        .unwrap_or(false);

    let invocation = format_invocation_line(&state, cwd);

    let Some(id) = state.tool_call_id.clone() else {
        // Missing toolCallId: best-effort output only, never stored.
        out.push_back(tool_call_text(invocation));
        if terminal {
            out.push_back(tool_call_text(format_result_line(&state)));
        }
        return;
    };

    map.insert(id.clone(), state.clone());
    out.push_back(tool_call_text(invocation));
    if terminal {
        out.push_back(tool_call_text(format_result_line(&state)));
        if !map.terminal_emitted(&id) {
            out.push_back(activity(&id, ToolCallActivityKind::Finish));
            map.mark_terminal_emitted(&id);
        }
        map.evict(&id);
    } else if !map.start_emitted(&id) {
        out.push_back(activity(&id, ToolCallActivityKind::Start));
        map.mark_start_emitted(&id);
    }
}

fn handle_tool_call_update(
    payload: ToolCallDisplayState,
    map: &mut ToolCallMap,
    out: &mut VecDeque<ClientUpdate>,
) {
    let terminal = payload
        .status
        .as_deref()
        .map(is_terminal_status)
        .unwrap_or(false);
    // Only an explicit `pending`/`in_progress` status counts as a Start
    // transition for the watchdog; an update without status is just a merge.
    let active = payload
        .status
        .as_deref()
        .is_some_and(|status| matches!(status, "pending" | "in_progress"));

    let Some(id) = payload.tool_call_id.clone() else {
        if terminal {
            out.push_back(tool_call_text(format_result_line(&payload)));
        }
        return;
    };

    if let Some(state) = map.merge(&id, &payload) {
        if terminal {
            out.push_back(tool_call_text(format_result_line(state)));
            if !map.terminal_emitted(&id) {
                out.push_back(activity(&id, ToolCallActivityKind::Finish));
                map.mark_terminal_emitted(&id);
            }
            map.evict(&id);
        } else if active && !map.start_emitted(&id) {
            out.push_back(activity(&id, ToolCallActivityKind::Start));
            map.mark_start_emitted(&id);
        }
    } else if terminal {
        if map.terminal_emitted(&id) {
            return;
        }
        out.push_back(tool_call_text(format_result_line(&payload)));
        out.push_back(activity(&id, ToolCallActivityKind::Finish));
        map.mark_terminal_emitted(&id);
    }
}

fn activity(id: &str, kind: ToolCallActivityKind) -> ClientUpdate {
    ClientUpdate::ToolCallActivity {
        tool_call_id: id.to_string(),
        kind,
    }
}

/// Tool-call invocation/result text is always tagged `StartNewMessage` so the
/// runner can finalize the live thought-stream block before appending the
/// synthetic paragraph.
fn tool_call_text(text: String) -> ClientUpdate {
    ClientUpdate::ToolCallText {
        text,
        boundary: AcpTextBoundary::StartNewMessage,
        identity: None,
    }
}
