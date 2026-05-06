//! Translate ACP `session/update` payloads into `ClientUpdate`s.
//!
//! Each text-bearing update carries an `AcpTextBoundary`. The classification
//! rule is:
//!
//! * `StartNewMessage` at every explicit boundary — session start, prompt-turn
//!   reset, or tool-call interleave — and whenever a stable identity changes.
//! * `Continue` otherwise: mid-stream no-identity chunks default to continuing
//!   the live block, and matching identities continue as well.

use super::super::{
    AcpTextBoundary, ClientUpdate, ToolCallActivityKind,
    tool_call::{
        ToolCallDisplayState, ToolCallMap, ToolCallPayload, format_invocation_line,
        format_result_line, is_terminal_status,
    },
};
use serde_json::Value;
use std::{collections::VecDeque, path::Path};

/// Per-stream identity + restart-flag tracking used to classify text chunks
/// as `Continue` vs. `StartNewMessage`.
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

/// Per-stream boundary classification state.
///
/// Real ACP servers emit `agent_message_chunk` events without any stable
/// message id, so the classifier defaults mid-stream chunks to `Continue` and
/// only emits `StartNewMessage` at explicit boundaries: the very first chunk
/// after session start, after a prompt-turn reset, or after a tool-call
/// interleave. When a payload does carry a stable id, the classifier honors
/// it: matching ids stay `Continue`, differing ids start a new message.
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

    /// Reset both streams at a prompt-turn boundary.
    ///
    /// ACP servers may legally reuse message ids across turns, so the next
    /// turn must always restart at `StartNewMessage` even when the first
    /// chunk repeats an earlier id.
    pub(super) fn reset_for_prompt_turn(&mut self) {
        self.agent_message = StreamIdentity::fresh();
        self.agent_thought = StreamIdentity::fresh();
    }

    /// Reset both streams so the next agent or thought chunk is classified
    /// as `StartNewMessage`. Called whenever a tool-call invocation/result
    /// interleaves the visible stream.
    fn reset_for_tool_call(&mut self) {
        self.reset_for_prompt_turn();
    }
}

/// Translate one ACP `session/update` payload into zero or more visible
/// `ClientUpdate`s, mutating the per-session tool-call state map and
/// boundary state in the process. A single `tool_call` payload may yield two
/// updates (invocation followed by result) when its status is already
/// terminal; non-terminal `tool_call_update`s with prior state are absorbed
/// silently and emit nothing.
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
        "agent_message_chunk" => {
            let text = value
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let identity = extract_message_identity(value);
            let boundary =
                boundary_for_text_chunk(&mut boundary_state.agent_message, identity.as_deref());
            out.push_back(ClientUpdate::AgentMessageText {
                text,
                boundary,
                identity,
            });
        }
        "agent_thought_chunk" => {
            let text = value
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let identity = extract_message_identity(value);
            let boundary =
                boundary_for_text_chunk(&mut boundary_state.agent_thought, identity.as_deref());
            out.push_back(ClientUpdate::AgentThoughtText {
                text,
                boundary,
                identity,
            });
        }
        "session_info_update" => {
            out.push_back(ClientUpdate::SessionInfoUpdate {
                title: value
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        "tool_call" => {
            // A tool-call invocation interleaves the visible stream and
            // therefore acts as a hard boundary for both agent and thought
            // streams. Any future free-form text gets `StartNewMessage` even
            // if it carries an identity we previously matched.
            boundary_state.reset_for_tool_call();
            handle_tool_call(ToolCallPayload::from_value(value), cwd, map, out);
        }
        "tool_call_update" => {
            // Mirror the `tool_call` behavior: a tool-call lifecycle update
            // (terminal or otherwise) prevents post-tool agent text from
            // gluing onto pre-tool live buffers.
            boundary_state.reset_for_tool_call();
            handle_tool_call_update(ToolCallPayload::from_value(value), map, out);
        }
        other => out.push_back(ClientUpdate::Unknown {
            kind: other.to_string(),
        }),
    }
}

/// Classify a single text chunk relative to the per-stream state we have
/// already observed.
///
/// `restart_pending` is the only source of `StartNewMessage` for no-identity
/// chunks: it is set at session start and at every explicit boundary
/// (prompt-turn reset, tool-call interleave), and cleared once a chunk has
/// been classified. Real ACP servers do not surface a stable message id on
/// `agent_message_chunk` events, so a no-identity mid-stream chunk defaults
/// to `Continue` rather than over-splitting one streamed response into one
/// persisted message per chunk.
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

/// Best-effort lookup of a stable ACP message identity on a `session/update`
/// payload. The ACP spec does not currently mandate a single field name, so
/// this checks the most plausible locations. Any future protocol revision
/// that surfaces a stable id should land here.
fn extract_message_identity(value: &Value) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "/messageId",
        "/message_id",
        "/id",
        "/content/messageId",
        "/content/message_id",
        "/content/id",
    ];
    for pointer in CANDIDATES {
        if let Some(id) = value.pointer(pointer).and_then(Value::as_str)
            && !id.is_empty()
        {
            return Some(id.to_string());
        }
    }
    None
}

fn handle_tool_call(
    payload: ToolCallPayload,
    cwd: &Path,
    map: &mut ToolCallMap,
    out: &mut VecDeque<ClientUpdate>,
) {
    let state = ToolCallDisplayState::from_payload(&payload);
    let terminal = state
        .status
        .as_deref()
        .map(is_terminal_status)
        .unwrap_or(false);

    let invocation = format_invocation_line(&state, cwd);

    if let Some(id) = payload.tool_call_id.clone() {
        map.insert(id.clone(), state.clone());
        out.push_back(tool_call_text(invocation));
        // Watchdog activity transitions: a `tool_call` payload represents a
        // freshly-observed tool-call id. If it is non-terminal (or missing
        // status, which we conservatively treat as in-flight), emit a Start
        // so the App can pause its idle clock from this moment. If it is
        // already terminal, skip Start — there was no observable in-flight
        // window for this id at the runner — and emit a single Finish.
        if terminal {
            out.push_back(tool_call_text(format_result_line(&state)));
            if !map.terminal_emitted(&id) {
                out.push_back(ClientUpdate::ToolCallActivity {
                    tool_call_id: id.clone(),
                    kind: ToolCallActivityKind::Finish,
                });
                map.mark_terminal_emitted(&id);
            }
            map.evict(&id);
        } else if !map.start_emitted(&id) {
            out.push_back(ClientUpdate::ToolCallActivity {
                tool_call_id: id.clone(),
                kind: ToolCallActivityKind::Start,
            });
            map.mark_start_emitted(&id);
        }
    } else {
        // Missing toolCallId: best-effort output only, never stored.
        // No watchdog activity emitted: dedup requires a stable id.
        out.push_back(tool_call_text(invocation));
        if terminal {
            out.push_back(tool_call_text(format_result_line(&state)));
        }
    }
}

fn handle_tool_call_update(
    payload: ToolCallPayload,
    map: &mut ToolCallMap,
    out: &mut VecDeque<ClientUpdate>,
) {
    let terminal = payload
        .status
        .as_deref()
        .map(is_terminal_status)
        .unwrap_or(false);
    // For watchdog activity tracking, only an explicit `pending` /
    // `in_progress` status on an update counts as a Start transition. An
    // update without an explicit status is just a property merge, not a
    // lifecycle signal, so we do not synthesize a Start from it.
    let active = payload
        .status
        .as_deref()
        .is_some_and(|status| matches!(status, "pending" | "in_progress"));

    let Some(id) = payload.tool_call_id.clone() else {
        // Missing toolCallId: best-effort result if terminal, otherwise drop.
        // No watchdog activity emitted: dedup requires a stable id.
        if terminal {
            let state = ToolCallDisplayState::from_payload(&payload);
            out.push_back(tool_call_text(format_result_line(&state)));
        }
        return;
    };

    if let Some(state) = map.merge(&id, &payload) {
        if terminal {
            let result = format_result_line(state);
            out.push_back(tool_call_text(result));
            if !map.terminal_emitted(&id) {
                out.push_back(ClientUpdate::ToolCallActivity {
                    tool_call_id: id.clone(),
                    kind: ToolCallActivityKind::Finish,
                });
                map.mark_terminal_emitted(&id);
            }
            map.evict(&id);
        } else if active && !map.start_emitted(&id) {
            // Server reported in_progress / pending without a prior
            // `tool_call`; treat this as the first observation of a
            // non-terminal status and emit a single Start.
            out.push_back(ClientUpdate::ToolCallActivity {
                tool_call_id: id.clone(),
                kind: ToolCallActivityKind::Start,
            });
            map.mark_start_emitted(&id);
        }
        // Non-terminal merges into prior state and produces no transcript
        // output (spec §Behavior rule 5).
    } else if terminal {
        if map.terminal_emitted(&id) {
            // Duplicate terminal update for an already-completed id: suppress
            // re-emission to keep the two-block contract append-only and to
            // satisfy the one-shot Finish contract for the watchdog.
            return;
        }
        // No prior state (never created or already evicted): emit a
        // best-effort result block from the payload only; never insert.
        let state = ToolCallDisplayState::from_payload(&payload);
        out.push_back(tool_call_text(format_result_line(&state)));
        out.push_back(ClientUpdate::ToolCallActivity {
            tool_call_id: id.clone(),
            kind: ToolCallActivityKind::Finish,
        });
        map.mark_terminal_emitted(&id);
    }
    // Non-terminal update with no prior state is silently dropped.
}

/// Build a `ClientUpdate::ToolCallText` with the boundary metadata required
/// by the runner. Tool-call invocation/result text is always tagged
/// `StartNewMessage` so the runner can finalize the thought stream's live
/// buffer before appending the synthetic paragraph and prevent post-tool
/// free-form text from gluing onto a pre-tool live buffer.
fn tool_call_text(text: String) -> ClientUpdate {
    ClientUpdate::ToolCallText {
        text,
        boundary: AcpTextBoundary::StartNewMessage,
        identity: None,
    }
}
