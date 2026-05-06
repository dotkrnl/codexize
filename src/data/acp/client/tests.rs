use super::super::actor::{RpcClient, client_request_response};
use super::super::dispatch::{AcpBoundaryState, dispatch_update};
use super::super::handshake::{
    PromptTurnOutcome, parse_initialize_result, parse_prompt_result, prompt_request_params,
};
use super::super::tool_call::TOOL_CALL_MAP_CAP;
use super::super::{AcpTextBoundary, PromptPayload, ToolCallActivityKind};
use super::*;

#[test]
fn parse_prompt_result_marks_failure_stop_reasons() {
    let result = parse_prompt_result(json!({ "stopReason": "interrupted" }))
        .expect("stop reason should parse");
    assert!(matches!(
        result,
        PromptTurnOutcome::Failed { message } if message.contains("interrupted")
    ));
}

#[test]
fn parse_prompt_result_accepts_success_stop_reasons() {
    let result =
        parse_prompt_result(json!({ "stopReason": "end_turn" })).expect("stop reason parsed");
    assert_eq!(result, PromptTurnOutcome::Finished);
}

#[test]
fn prompt_request_params_include_uuid_message_id() {
    let params = prompt_request_params("sess-1", &PromptPayload::Text("hello".to_string()))
        .expect("prompt params");
    let message_id = params
        .get("messageId")
        .and_then(Value::as_str)
        .expect("messageId");

    assert_eq!(message_id.len(), 36);
    assert_eq!(message_id.chars().filter(|ch| *ch == '-').count(), 4);
    assert_eq!(
        params.get("sessionId").and_then(Value::as_str),
        Some("sess-1")
    );
    assert_eq!(
        params.pointer("/prompt/0/text").and_then(Value::as_str),
        Some("hello")
    );
}

#[test]
fn raw_acp_update_trace_records_payload() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let trace_path = temp.path().join("run.acp.jsonl");
    let update = json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "text": "Proposed correction" }
    });

    append_raw_acp_update_trace(Some(&trace_path), &update);

    let trace = std::fs::read_to_string(trace_path).expect("trace file");
    assert!(trace.contains(r#""type":"raw_update""#));
    assert!(trace.contains(r#""sessionUpdate":"agent_message_chunk""#));
    assert!(trace.contains(r#""text":"Proposed correction""#));
}

fn drain(value: Value, cwd: &Path, map: &mut ToolCallMap) -> Vec<ClientUpdate> {
    let mut state = AcpBoundaryState::new();
    let mut out = VecDeque::new();
    dispatch_update(&value, cwd, map, &mut state, &mut out);
    out.into_iter().collect()
}

fn drain_with_state(
    value: Value,
    cwd: &Path,
    map: &mut ToolCallMap,
    state: &mut AcpBoundaryState,
) -> Vec<ClientUpdate> {
    let mut out = VecDeque::new();
    dispatch_update(&value, cwd, map, state, &mut out);
    out.into_iter().collect()
}

fn tool_call_block(text: &str) -> ClientUpdate {
    ClientUpdate::ToolCallText {
        text: text.to_string(),
        boundary: AcpTextBoundary::StartNewMessage,
        identity: None,
    }
}

fn activity(id: &str, kind: ToolCallActivityKind) -> ClientUpdate {
    ClientUpdate::ToolCallActivity {
        tool_call_id: id.to_string(),
        kind,
    }
}

#[test]
fn dispatch_renders_invocation_from_observed_codex_read_payload() {
    let mut map = ToolCallMap::new();
    let updates = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "Read Cargo.toml",
            "kind": "read",
            "status": "in_progress",
            "locations": [{ "path": "/work/project/Cargo.toml" }],
            "rawInput": {
                "command": ["/bin/zsh", "-lc", "sed -n '1,120p' Cargo.toml"]
            }
        }),
        Path::new("/work/project"),
        &mut map,
    );

    assert_eq!(
        updates,
        vec![
            tool_call_block("tool: read(Cargo.toml)"),
            activity("call_1", ToolCallActivityKind::Start),
        ]
    );
    assert_eq!(map.len(), 1);
}

#[test]
fn dispatch_emits_invocation_then_result_when_terminal_arrives_in_two_payloads() {
    // Spec §Behavior: the invocation block is emitted on `tool_call`, and
    // a separate result block is emitted on the terminal `tool_call_update`.
    let mut map = ToolCallMap::new();
    let invocation = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "Read Cargo.toml",
            "kind": "read",
            "status": "in_progress",
            "locations": [{ "path": "/work/project/Cargo.toml" }],
        }),
        Path::new("/work/project"),
        &mut map,
    );
    assert_eq!(
        invocation,
        vec![
            tool_call_block("tool: read(Cargo.toml)"),
            activity("call_1", ToolCallActivityKind::Start),
        ]
    );
    assert!(map.contains("call_1"));

    let result = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "[package] name = \"codexize\"" }
        }),
        Path::new("/work/project"),
        &mut map,
    );
    assert_eq!(
        result,
        vec![
            tool_call_block("result: completed, exit 0, output: [package] name = \"codexize\""),
            activity("call_1", ToolCallActivityKind::Finish),
        ]
    );
    // After eviction the entry must be gone.
    assert!(!map.contains("call_1"));
}

#[test]
fn dispatch_emits_invocation_and_result_when_tool_call_payload_is_already_terminal() {
    // Spec §Behavior rule 1: a `tool_call` carrying terminal status emits
    // the invocation followed by the result, then evicts.
    let mut map = ToolCallMap::new();
    let updates = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_q",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "command": ["echo", "ok"] },
            "rawOutput": { "exit_code": 0, "stdout": "ok" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        updates,
        vec![
            tool_call_block("tool: exec(echo ok)"),
            tool_call_block("result: completed, exit 0, output: ok"),
            activity("call_q", ToolCallActivityKind::Finish),
        ]
    );
    assert!(!map.contains("call_q"));
}

#[test]
fn dispatch_silently_merges_non_terminal_update_into_existing_state() {
    // Spec §Behavior rule 5: non-terminal `tool_call_update` events
    // produce no transcript output but still merge into the merge state,
    // so a later terminal update can use the merged status snapshot.
    let mut map = ToolCallMap::new();
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "kind": "execute",
            "rawInput": { "command": ["sleep", "1"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let progress = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "in_progress",
            "rawOutput": { "stdout": "still working" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert!(
        progress.is_empty(),
        "non-terminal updates must produce no visible blocks"
    );
    let merged = map.get("call_1").expect("entry preserved");
    assert_eq!(merged.status.as_deref(), Some("in_progress"));
}

#[test]
fn dispatch_terminal_update_without_prior_state_emits_best_effort_result_only() {
    // Spec §Behavior rule 4: terminal update with no prior state renders
    // a result block from the payload alone, with no synthesized
    // invocation and no map entry retained.
    let mut map = ToolCallMap::new();
    let updates = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "stale_id",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "ok" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        updates,
        vec![
            tool_call_block("result: completed, exit 0, output: ok"),
            activity("stale_id", ToolCallActivityKind::Finish),
        ]
    );
    assert!(
        !map.contains("stale_id"),
        "best-effort updates must never insert state"
    );
}

#[test]
fn dispatch_second_terminal_update_for_evicted_id_is_suppressed() {
    // Once a terminal result has been emitted for an id, later terminal
    // updates for that same id are ignored unless a new `tool_call`
    // reuses the id and starts a fresh lifecycle.
    let mut map = ToolCallMap::new();
    let _invocation = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "kind": "execute",
            "rawInput": { "command": ["echo", "hi"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let first_result = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "hi" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    // Result block + watchdog Finish transition.
    assert_eq!(
        first_result,
        vec![
            tool_call_block("result: completed, exit 0, output: hi"),
            activity("call_1", ToolCallActivityKind::Finish),
        ]
    );
    assert!(!map.contains("call_1"));

    // Duplicate terminal update for the now-evicted id must be ignored.
    let second_result = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "stale" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert!(second_result.is_empty());
    assert!(!map.contains("call_1"));

    // A non-terminal stale update produces nothing.
    let stale = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "in_progress"
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert!(stale.is_empty());
}

#[test]
fn dispatch_id_reuse_after_terminal_renders_text_without_repeating_activity() {
    // Id reuse is permitted for the *display* contract (a fresh
    // invocation block + a fresh result block render under the reused
    // id), but the watchdog activity stream is one-shot per id within a
    // session: a previously-completed tool_call_id must not be allowed
    // to resurrect Start or Finish — doing so would let the App's idle
    // clock pause/resume on a phantom call.
    let mut map = ToolCallMap::new();
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "kind": "execute",
            "rawInput": { "command": ["echo", "first"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "rawOutput": { "stdout": "first" }
        }),
        Path::new("/tmp"),
        &mut map,
    );

    let reused_invocation = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "kind": "execute",
            "rawInput": { "command": ["echo", "second"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        reused_invocation,
        vec![tool_call_block("tool: exec(echo second)")],
        "reused id must not re-emit Start"
    );
    let reused_result = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "rawOutput": { "stdout": "second" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        reused_result,
        vec![tool_call_block("result: completed, output: second")],
        "reused id must not re-emit Finish"
    );
}

#[test]
fn dispatch_renders_exec_invocation_from_command_array() {
    let mut map = ToolCallMap::new();
    let updates = drain(
        json!({
            "sessionUpdate": "tool_call",
            "rawInput": { "command": ["cargo", "test", "--workspace"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        updates,
        vec![tool_call_block("tool: exec(cargo test --workspace)")]
    );
    // Missing toolCallId must never be stored.
    assert_eq!(map.len(), 0);
}

#[test]
fn dispatch_falls_back_to_literal_tool_when_payload_is_empty() {
    let mut map = ToolCallMap::new();
    let updates = drain(
        json!({ "sessionUpdate": "tool_call" }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(updates, vec![tool_call_block("tool: tool")]);
}

fn activity_only(updates: Vec<ClientUpdate>) -> Vec<ClientUpdate> {
    updates
        .into_iter()
        .filter(|update| matches!(update, ClientUpdate::ToolCallActivity { .. }))
        .collect()
}

#[test]
fn activity_dedup_repeated_pending_in_progress_emits_single_start() {
    let mut map = ToolCallMap::new();
    let initial = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_dup",
            "kind": "execute",
            "status": "pending",
            "rawInput": { "command": ["sleep", "1"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert_eq!(
        initial,
        vec![activity("call_dup", ToolCallActivityKind::Start)]
    );

    let progress = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_dup",
            "status": "in_progress"
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        progress.is_empty(),
        "second non-terminal status must not re-emit Start"
    );

    let still_progress = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_dup",
            "status": "in_progress",
            "rawOutput": { "stdout": "tick" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        still_progress.is_empty(),
        "third non-terminal status must not re-emit Start"
    );
}

#[test]
fn activity_dedup_terminal_then_terminal_emits_single_finish() {
    let mut map = ToolCallMap::new();
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_t",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["sleep", "1"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let first_terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_t",
            "status": "failed",
            "rawOutput": { "exit_code": 1, "stderr": "boom" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert_eq!(
        first_terminal,
        vec![activity("call_t", ToolCallActivityKind::Finish)]
    );

    let second_terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_t",
            "status": "cancelled"
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        second_terminal.is_empty(),
        "second terminal status for the same id must not re-emit Finish"
    );

    let third_terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_t",
            "status": "errored"
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        third_terminal.is_empty(),
        "later terminal statuses must remain suppressed"
    );
}

#[test]
fn activity_dedup_repeated_tool_call_payload_emits_single_start() {
    let mut map = ToolCallMap::new();
    let initial = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_repeat",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["sleep", "1"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert_eq!(
        initial,
        vec![activity("call_repeat", ToolCallActivityKind::Start)]
    );

    let resent = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_repeat",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["sleep", "1"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        resent.is_empty(),
        "repeated tool_call payload must not re-emit Start"
    );

    let resent_pending = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_repeat",
            "kind": "execute",
            "status": "pending",
            "rawInput": { "command": ["sleep", "1"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        resent_pending.is_empty(),
        "second non-terminal tool_call payload must not re-emit Start"
    );
}

#[test]
fn activity_dedup_repeated_terminal_tool_call_payload_emits_single_finish() {
    let mut map = ToolCallMap::new();
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_dt",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["echo", "x"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let first_terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_dt",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "command": ["echo", "x"] },
            "rawOutput": { "exit_code": 0, "stdout": "x" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert_eq!(
        first_terminal,
        vec![activity("call_dt", ToolCallActivityKind::Finish)]
    );

    let second_terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_dt",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "command": ["echo", "x"] },
            "rawOutput": { "exit_code": 0, "stdout": "x" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        second_terminal.is_empty(),
        "repeated terminal tool_call payload must not re-emit Finish"
    );

    let third_terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_dt",
            "status": "errored"
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        third_terminal.is_empty(),
        "tool_call_update terminal after duplicate-terminal tool_call must remain suppressed"
    );
}

#[test]
fn activity_dedup_terminal_tool_call_after_completion_does_not_resurrect_finish() {
    let mut map = ToolCallMap::new();
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_echo",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["echo", "y"] }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let _ = drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_echo",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "y" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    let resurrected = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_echo",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "command": ["echo", "y"] },
            "rawOutput": { "exit_code": 0, "stdout": "y" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        resurrected.is_empty(),
        "terminal tool_call payload for an already-finished id must not re-emit Finish"
    );
}

#[test]
fn activity_dedup_survives_more_than_cap_distinct_ids_for_full_session() {
    let mut map = ToolCallMap::new();

    for i in 0..(TOOL_CALL_MAP_CAP + 5) {
        let id = format!("call_{i}");
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": id,
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["echo", id] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": id }
            }),
            Path::new("/tmp"),
            &mut map,
        );
    }

    assert!(!map.contains("call_0"));

    let resent_start = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_0",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["echo", "call_0"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        resent_start.is_empty(),
        "Start must not re-emit for id whose marker was set before the FIFO cap was exceeded"
    );

    let resent_finish = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_0",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "call_0" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(
        resent_finish.is_empty(),
        "Finish must not re-emit for id whose marker was set before the FIFO cap was exceeded"
    );
}

#[test]
fn activity_distinct_ids_are_tracked_independently_with_correct_ordering() {
    let mut map = ToolCallMap::new();

    let start_a = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_a",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["echo", "a"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    let start_b = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_b",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": { "command": ["echo", "b"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    let finish_a = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_a",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "a" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    let finish_b = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_b",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "b" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));

    assert_eq!(
        start_a,
        vec![activity("call_a", ToolCallActivityKind::Start)]
    );
    assert_eq!(
        start_b,
        vec![activity("call_b", ToolCallActivityKind::Start)]
    );
    assert_eq!(
        finish_a,
        vec![activity("call_a", ToolCallActivityKind::Finish)]
    );
    assert_eq!(
        finish_b,
        vec![activity("call_b", ToolCallActivityKind::Finish)]
    );
}

#[test]
fn activity_terminal_directly_emits_only_finish_no_start() {
    let mut map = ToolCallMap::new();
    let updates = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "fast_call",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "command": ["true"] },
            "rawOutput": { "exit_code": 0 }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert_eq!(
        updates,
        vec![activity("fast_call", ToolCallActivityKind::Finish)]
    );
}

#[test]
fn activity_missing_tool_call_id_emits_no_transitions() {
    let mut map = ToolCallMap::new();
    let initial = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call",
            "kind": "execute",
            "rawInput": { "command": ["echo", "ghost"] }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(initial.is_empty());

    let terminal = activity_only(drain(
        json!({
            "sessionUpdate": "tool_call_update",
            "status": "completed",
            "rawOutput": { "exit_code": 0, "stdout": "ghost" }
        }),
        Path::new("/tmp"),
        &mut map,
    ));
    assert!(terminal.is_empty());
}

#[test]
fn dispatch_routes_unrelated_kinds_containing_tool_to_unknown() {
    let mut map = ToolCallMap::new();
    let updates = drain(
        json!({ "sessionUpdate": "tool_progress_chunk" }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        updates,
        vec![ClientUpdate::Unknown {
            kind: "tool_progress_chunk".to_string()
        }]
    );
}

#[test]
fn dispatch_emits_session_update_unknown_when_payload_is_null() {
    let mut map = ToolCallMap::new();
    let updates = drain(Value::Null, Path::new("/tmp"), &mut map);
    assert_eq!(
        updates,
        vec![ClientUpdate::Unknown {
            kind: "session/update".to_string()
        }]
    );
}

#[test]
fn dispatch_passes_through_agent_message_and_thought_chunks() {
    let mut map = ToolCallMap::new();
    let messages = drain(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "hello" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        messages,
        vec![ClientUpdate::AgentMessageText {
            text: "hello".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }]
    );

    let thoughts = drain(
        json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "text": "thinking" }
        }),
        Path::new("/tmp"),
        &mut map,
    );
    assert_eq!(
        thoughts,
        vec![ClientUpdate::AgentThoughtText {
            text: "thinking".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }]
    );
}

#[test]
fn dispatch_continues_no_identity_chunks_until_explicit_boundary() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let first = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "first " }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    let second = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "second" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert_eq!(
        first,
        vec![ClientUpdate::AgentMessageText {
            text: "first ".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }]
    );
    assert_eq!(
        second,
        vec![ClientUpdate::AgentMessageText {
            text: "second".to_string(),
            boundary: AcpTextBoundary::Continue,
            identity: None,
        }]
    );
}

#[test]
fn dispatch_restarts_no_identity_chunk_after_tool_call_interleave() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let _ = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "before" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    let _ = drain_with_state(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_x",
            "kind": "execute",
            "rawInput": { "command": ["echo", "x"] }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    let after = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "after" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert_eq!(
        after,
        vec![ClientUpdate::AgentMessageText {
            text: "after".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }]
    );
}

#[test]
fn dispatch_restarts_no_identity_chunk_across_prompt_turns() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let _ = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "turn one" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    state.reset_for_prompt_turn();
    let next_turn = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "turn two" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert_eq!(
        next_turn,
        vec![ClientUpdate::AgentMessageText {
            text: "turn two".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }]
    );
}

#[test]
fn dispatch_emits_continue_when_message_identity_persists() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let first = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-7",
            "content": { "text": "hel" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    let second = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-7",
            "content": { "text": "lo" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert_eq!(
        first,
        vec![ClientUpdate::AgentMessageText {
            text: "hel".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: Some("msg-7".to_string()),
        }]
    );
    assert_eq!(
        second,
        vec![ClientUpdate::AgentMessageText {
            text: "lo".to_string(),
            boundary: AcpTextBoundary::Continue,
            identity: Some("msg-7".to_string()),
        }]
    );
}

#[test]
fn dispatch_resets_continuation_after_tool_call_interleave() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let _ = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-7",
            "content": { "text": "before" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    let _ = drain_with_state(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_x",
            "kind": "execute",
            "rawInput": { "command": ["echo", "x"] }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    let after = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-7",
            "content": { "text": "after" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert_eq!(
        after,
        vec![ClientUpdate::AgentMessageText {
            text: "after".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: Some("msg-7".to_string()),
        }]
    );
}

#[test]
fn dispatch_resets_continuation_across_prompt_turns() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let _ = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-7",
            "content": { "text": "turn one" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    state.reset_for_prompt_turn();
    let next_turn = drain_with_state(
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-7",
            "content": { "text": "turn two" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert_eq!(
        next_turn,
        vec![ClientUpdate::AgentMessageText {
            text: "turn two".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: Some("msg-7".to_string()),
        }]
    );
}

#[test]
fn dispatch_tool_call_text_is_always_start_new_message() {
    let mut map = ToolCallMap::new();
    let mut state = AcpBoundaryState::new();
    let updates = drain_with_state(
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_y",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "command": ["echo", "ok"] },
            "rawOutput": { "exit_code": 0, "stdout": "ok" }
        }),
        Path::new("/tmp"),
        &mut map,
        &mut state,
    );
    assert!(updates.iter().all(|update| matches!(
        update,
        ClientUpdate::ToolCallText {
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
            ..
        } | ClientUpdate::ToolCallActivity { .. }
    )));
    assert!(
        updates
            .iter()
            .any(|update| matches!(update, ClientUpdate::ToolCallText { .. })),
        "tool call should have produced at least one text block"
    );
}

#[test]
fn permission_request_selects_approve_option() {
    let response = client_request_response(
        "session/request_permission",
        &json!({
            "options": [
                { "optionId": "approve", "kind": "allow_once" },
                { "optionId": "reject", "kind": "reject_once" }
            ]
        }),
    )
    .expect("permission request should be handled");

    assert_eq!(
        response,
        json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "approve"
            }
        })
    );
}

// === Async transport coverage ===
//
// These tests pin the JSON wire shape (request id, response correlation,
// notifications, server-initiated request handling, EOF / cancel paths) by
// running the actor directly against in-memory `tokio::io::duplex` streams.
// A real subprocess would exercise the same code path; the duplex pair
// removes the OS-level dependency so the suite stays hermetic.

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

fn server_pair() -> (
    BufReader<tokio::io::DuplexStream>,
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
) {
    let (client_stdout, server_stdout) = tokio::io::duplex(8192);
    let (server_stdin, client_stdin) = tokio::io::duplex(8192);
    (
        BufReader::new(client_stdout),
        client_stdin,
        server_stdin,
        server_stdout,
    )
}

async fn read_server_line(server_in: &mut tokio::io::DuplexStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = server_in
            .read(&mut byte)
            .await
            .expect("server read line should succeed");
        if n == 0 {
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    String::from_utf8(buf).expect("utf-8 line")
}

fn test_runtime() -> std::sync::Arc<tokio::runtime::Runtime> {
    std::sync::Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("test runtime"),
    )
}

#[test]
fn transcript_replay_request_response_correlation() {
    let runtime = test_runtime();
    let runtime_for_actor = runtime.clone();
    runtime.block_on(async move {
        let (client_reader, client_writer, mut server_in, mut server_out) = server_pair();
        let mut rpc = RpcClient::start(runtime_for_actor, client_reader, client_writer);

        // Drive a request through the actor; concurrently impersonate the server.
        let server = tokio::spawn(async move {
            let line = read_server_line(&mut server_in).await;
            let request: Value = serde_json::from_str(&line).expect("JSON request");
            assert_eq!(request["method"], "initialize");
            assert_eq!(request["id"], 0);
            assert_eq!(request["jsonrpc"], "2.0");
            assert_eq!(request["params"]["protocolVersion"], 1);
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "sessionCapabilities": { "close": true }
                    }
                }
            });
            let mut bytes = serde_json::to_vec(&response).expect("encode");
            bytes.push(b'\n');
            server_out.write_all(&bytes).await.expect("server write");
        });

        let response = rpc
            .call_async("initialize", json!({ "protocolVersion": 1 }))
            .await
            .expect("client receives response");
        server.await.expect("server task");

        let init = parse_initialize_result(response).expect("parse initialize");
        assert_eq!(init.protocol_version, 1);
        assert!(init.supports_close);
    });
}

#[test]
fn transcript_replay_session_update_notifications_drain_through_actor() {
    let runtime = test_runtime();
    let runtime_for_actor = runtime.clone();
    runtime.block_on(async move {
        let (client_reader, client_writer, _server_in, mut server_out) = server_pair();
        let mut rpc = RpcClient::start(runtime_for_actor, client_reader, client_writer);

        let payload = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-7",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "text": "hi" }
                }
            }
        });
        let mut bytes = serde_json::to_vec(&payload).expect("encode");
        bytes.push(b'\n');
        server_out.write_all(&bytes).await.expect("server write");

        // Give the actor a moment to process the inbound line on its worker
        // thread; we yield repeatedly rather than racing on a sleep.
        let value = loop {
            match rpc.try_next_update() {
                Ok(Some(value)) => break value,
                _ => tokio::task::yield_now().await,
            }
        };

        assert_eq!(
            value.pointer("/sessionUpdate").and_then(Value::as_str),
            Some("agent_message_chunk")
        );
    });
}

#[test]
fn transcript_replay_server_request_is_answered_with_permission_response() {
    let runtime = test_runtime();
    let runtime_for_actor = runtime.clone();
    runtime.block_on(async move {
        let (client_reader, client_writer, mut server_in, mut server_out) = server_pair();
        let _rpc = RpcClient::start(runtime_for_actor, client_reader, client_writer);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/request_permission",
            "params": {
                "options": [
                    { "optionId": "approve", "kind": "allow_once" },
                    { "optionId": "reject", "kind": "reject_once" }
                ]
            }
        });
        let mut bytes = serde_json::to_vec(&request).expect("encode");
        bytes.push(b'\n');
        server_out.write_all(&bytes).await.expect("server write");

        let line = read_server_line(&mut server_in).await;
        let response: Value = serde_json::from_str(&line).expect("response JSON");
        assert_eq!(response["id"], 7);
        assert_eq!(
            response.pointer("/result/outcome/optionId"),
            Some(&Value::String("approve".to_string()))
        );
    });
}

#[test]
fn actor_shutdown_flushes_queued_requests_before_exit() {
    let runtime = std::sync::Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime"),
    );
    let (client_reader, client_writer, mut server_in, _server_out) = server_pair();
    let mut rpc = RpcClient::start(runtime.clone(), client_reader, client_writer);

    let _ignored_response = rpc
        .start_request("session/close", json!({ "sessionId": "sess-queued" }))
        .expect("close request enqueued");
    rpc.shutdown_async(&runtime);

    runtime.block_on(async move {
        let line = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_server_line(&mut server_in),
        )
        .await
        .expect("queued close request should be flushed before actor shutdown");
        let request: Value = serde_json::from_str(&line).expect("close request JSON");
        assert_eq!(request["method"], "session/close");
        assert_eq!(request["params"]["sessionId"], "sess-queued");
    });
}

#[test]
fn actor_preserves_partial_inbound_line_when_command_interleaves() {
    let runtime = std::sync::Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime"),
    );
    let (client_reader, client_writer, mut server_in, mut server_out) = server_pair();
    let mut rpc = RpcClient::start(runtime.clone(), client_reader, client_writer);
    let update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "hello" }
            }
        }
    });
    let mut update_line = serde_json::to_string(&update).expect("update JSON");
    update_line.push('\n');
    let split_at = update_line.find("hello").expect("fixture contains text") + 3;
    let (partial_update, update_remainder) = update_line.split_at(split_at);

    runtime.block_on(async {
        server_out
            .write_all(partial_update.as_bytes())
            .await
            .expect("partial update write");
        tokio::task::yield_now().await;
    });

    let response = rpc
        .start_request("initialize", json!({ "protocolVersion": 1 }))
        .expect("request enqueued");
    runtime.block_on(async {
        let line = read_server_line(&mut server_in).await;
        let request: Value = serde_json::from_str(&line).expect("request JSON");
        assert_eq!(request["method"], "initialize");
        server_out
            .write_all(update_remainder.as_bytes())
            .await
            .expect("update remainder write");
    });
    let value = runtime.block_on(async {
        loop {
            match rpc.try_next_update() {
                Ok(Some(value)) => break value,
                Ok(None) => tokio::task::yield_now().await,
                Err(err) => panic!("partial update should remain valid after command: {err}"),
            }
        }
    });
    assert_eq!(
        value.pointer("/content/text").and_then(Value::as_str),
        Some("hello")
    );

    runtime.block_on(async {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": { "protocolVersion": 1 }
        });
        let mut bytes = serde_json::to_vec(&response).expect("response JSON");
        bytes.push(b'\n');
        server_out.write_all(&bytes).await.expect("response write");
    });
    let response = runtime
        .block_on(response)
        .expect("response channel")
        .expect("response result");
    assert_eq!(response["protocolVersion"], 1);
}
