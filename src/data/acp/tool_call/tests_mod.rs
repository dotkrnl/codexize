use super::*;
use serde_json::json;

fn state_from_payload(value: &Value) -> ToolCallDisplayState {
    ToolCallDisplayState::from_payload(&ToolCallPayload::from_value(value))
}

#[test]
fn parses_observed_codex_read_payload() {
    let payload = ToolCallPayload::from_value(&json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call_1",
        "title": "Read Cargo.toml",
        "kind": "read",
        "status": "in_progress",
        "locations": [{ "path": "/work/project/Cargo.toml" }],
        "rawInput": {
            "command": ["/bin/zsh", "-lc", "sed -n '1,120p' Cargo.toml"]
        }
    }));

    assert_eq!(payload.tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(payload.kind.as_deref(), Some("read"));
    assert_eq!(payload.status.as_deref(), Some("in_progress"));
    assert_eq!(payload.locations.len(), 1);
}

#[test]
fn invocation_renders_codex_read_call_with_relative_path() {
    let state = state_from_payload(&json!({
        "sessionUpdate": "tool_call",
        "kind": "read",
        "locations": [{ "path": "/work/project/Cargo.toml" }],
    }));
    let line = format_invocation_line(&state, Path::new("/work/project"));
    assert_eq!(line, "tool: read(Cargo.toml)");
}

#[test]
fn invocation_falls_back_to_basename_for_paths_outside_cwd() {
    let state = state_from_payload(&json!({
        "kind": "read",
        "locations": [{ "path": "/etc/hosts" }],
    }));
    let line = format_invocation_line(&state, Path::new("/work/project"));
    assert_eq!(line, "tool: read(hosts)");
}

#[test]
fn invocation_handles_multiple_read_locations() {
    let state = state_from_payload(&json!({
        "kind": "read",
        "locations": [
            { "path": "/work/project/a.rs" },
            { "path": "/work/project/b.rs" },
            { "path": "/work/project/c.rs" }
        ],
    }));
    let line = format_invocation_line(&state, Path::new("/work/project"));
    assert_eq!(line, "tool: read(a.rs, +2 more)");
}

#[test]
fn invocation_renders_exec_from_lc_script() {
    let state = state_from_payload(&json!({
        "kind": "execute",
        "rawInput": {
            "command": ["/bin/zsh", "-lc", "sed -n '1,120p' Cargo.toml"]
        }
    }));
    let line = format_invocation_line(&state, Path::new("/work/project"));
    assert_eq!(line, "tool: exec(sed -n '1,120p' Cargo.toml)");
}

#[test]
fn invocation_renders_exec_from_joined_command() {
    let state = state_from_payload(&json!({
        "rawInput": {
            "command": ["cargo", "test", "--workspace"]
        }
    }));
    let line = format_invocation_line(&state, Path::new("/tmp"));
    assert_eq!(line, "tool: exec(cargo test --workspace)");
}

#[test]
fn invocation_renders_other_kind_with_location() {
    let state = state_from_payload(&json!({
        "kind": "edit",
        "locations": [{ "path": "/work/project/src/lib.rs" }],
    }));
    let line = format_invocation_line(&state, Path::new("/work/project"));
    assert_eq!(line, "tool: edit(src/lib.rs)");
}

#[test]
fn invocation_falls_back_to_title_verbatim() {
    let state = state_from_payload(&json!({
        "title": "Search Workspace",
    }));
    let line = format_invocation_line(&state, Path::new("/tmp"));
    assert_eq!(line, "tool: Search Workspace");
}

#[test]
fn invocation_falls_back_to_literal_tool_when_empty() {
    let state = state_from_payload(&json!({}));
    let line = format_invocation_line(&state, Path::new("/tmp"));
    assert_eq!(line, "tool: tool");
}

#[test]
fn invocation_truncates_long_lc_scripts_to_under_200_chars() {
    let mut script = String::from("echo ");
    script.push_str(&"abcdefghij".repeat(40));
    let state = state_from_payload(&json!({
        "kind": "execute",
        "rawInput": { "command": ["/bin/zsh", "-lc", script] }
    }));
    let line = format_invocation_line(&state, Path::new("/tmp"));
    assert!(line.chars().count() <= INVOCATION_LINE_MAX);
    assert!(line.ends_with("..."));
    assert!(line.starts_with("tool: exec("));
}

#[test]
fn result_line_includes_status_exit_and_output() {
    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": {
            "exit_code": 0,
            "stdout": "[package]\nname = \"codexize\""
        }
    }));
    let line = format_result_line(&state);
    assert_eq!(
        line,
        "result: completed, exit 0, output: [package] name = \"codexize\""
    );
}

#[test]
fn result_line_prefers_stderr_on_failure_and_includes_exit_code() {
    let state = state_from_payload(&json!({
        "status": "failed",
        "rawOutput": {
            "exit_code": 101,
            "stdout": "compiling...",
            "stderr": "error[E0277]: missing trait impl"
        }
    }));
    let line = format_result_line(&state);
    assert_eq!(
        line,
        "result: failed, exit 101, stderr: error[E0277]: missing trait impl"
    );
}

#[test]
fn result_line_omits_clause_when_no_output_present() {
    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": { "exit_code": 0 }
    }));
    let line = format_result_line(&state);
    assert_eq!(line, "result: completed, exit 0");
}

#[test]
fn result_line_uses_first_text_content_when_raw_output_missing() {
    let state = state_from_payload(&json!({
        "status": "completed",
        "content": [{ "text": "from content block" }],
    }));
    let line = format_result_line(&state);
    assert_eq!(line, "result: completed, output: from content block");
}

#[test]
fn result_line_truncates_long_stdout_snippet() {
    let stdout = "x".repeat(2048);
    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": { "stdout": stdout }
    }));
    let line = format_result_line(&state);
    let prefix = "result: completed, output: ";
    assert!(line.starts_with(prefix));
    let snippet = &line[prefix.len()..];
    assert_eq!(snippet.chars().count(), SNIPPET_MAX_CHARS);
    assert!(snippet.ends_with("..."));
}

#[test]
fn result_line_collapses_whitespace_in_long_stdout() {
    let mut stdout = String::new();
    for i in 0..256 {
        stdout.push_str(&format!("line {i}\nmore\twhitespace\r"));
    }
    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": { "stdout": stdout }
    }));
    let line = format_result_line(&state);
    assert!(!line.contains('\n'));
    assert!(!line.contains('\t'));
    assert!(!line.contains('\r'));
    assert!(!line.contains("  "));
}

#[test]
fn sanitize_strips_ansi_csi_and_osc_sequences() {
    let dirty = "\u{1B}[31merror\u{1B}[0m \u{1B}]0;title\u{07}done";
    assert_eq!(sanitize_snippet(dirty), "error done");
}

#[test]
fn sanitize_strips_ansi_in_stderr_snippet() {
    let state = state_from_payload(&json!({
        "status": "failed",
        "rawOutput": {
            "exit_code": 1,
            "stderr": "\u{1B}[31mfatal:\u{1B}[0m broken pipe"
        }
    }));
    let line = format_result_line(&state);
    assert_eq!(line, "result: failed, exit 1, stderr: fatal: broken pipe");
}

#[test]
fn sanitize_replaces_control_chars_and_trims() {
    let dirty = "  \x01hello\x02 \x7Fworld\x00  ";
    assert_eq!(sanitize_snippet(dirty), "hello world");
}

#[test]
fn truncate_respects_utf8_boundaries() {
    let s = "é".repeat(200);
    let truncated = truncate_with_ellipsis(&s, 50);
    assert!(truncated.is_char_boundary(truncated.len()));
    assert_eq!(truncated.chars().count(), 50);
    assert!(truncated.ends_with("..."));
}

#[test]
fn truncate_returns_input_when_within_cap() {
    let s = "short";
    assert_eq!(truncate_with_ellipsis(s, 50), "short");
}

#[test]
fn snippet_uses_formatted_then_aggregated_then_stdout() {
    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": {
            "formatted_output": "FORMATTED",
            "aggregated_output": "AGGREGATED",
            "stdout": "STDOUT"
        }
    }));
    assert!(format_result_line(&state).ends_with("output: FORMATTED"));

    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": {
            "aggregated_output": "AGGREGATED",
            "stdout": "STDOUT"
        }
    }));
    assert!(format_result_line(&state).ends_with("output: AGGREGATED"));

    let state = state_from_payload(&json!({
        "status": "completed",
        "rawOutput": { "stdout": "STDOUT" }
    }));
    assert!(format_result_line(&state).ends_with("output: STDOUT"));
}

#[test]
fn merge_preserves_fields_from_earlier_payload() {
    let mut state = state_from_payload(&json!({
        "kind": "read",
        "title": "Read Cargo.toml",
        "locations": [{ "path": "/work/project/Cargo.toml" }],
    }));
    let update = ToolCallPayload::from_value(&json!({
        "status": "completed",
        "rawOutput": { "exit_code": 0, "stdout": "ok" }
    }));
    state.merge(&update);
    assert_eq!(state.kind.as_deref(), Some("read"));
    assert_eq!(state.title.as_deref(), Some("Read Cargo.toml"));
    assert_eq!(state.status.as_deref(), Some("completed"));
    assert_eq!(state.locations.len(), 1);
}

#[test]
fn merge_does_not_erase_fields_with_null_payload() {
    let mut state = state_from_payload(&json!({
        "kind": "read",
        "title": "first"
    }));
    let update = ToolCallPayload::from_value(&json!({
        "title": null,
        "kind": null,
        "status": "completed"
    }));
    state.merge(&update);
    assert_eq!(state.title.as_deref(), Some("first"));
    assert_eq!(state.kind.as_deref(), Some("read"));
    assert_eq!(state.status.as_deref(), Some("completed"));
}

#[test]
fn tool_call_map_evicts_oldest_when_cap_exceeded() {
    let mut map = ToolCallMap::new();
    for i in 0..TOOL_CALL_MAP_CAP {
        map.insert(format!("id-{i}"), ToolCallDisplayState::default());
    }
    assert_eq!(map.len(), TOOL_CALL_MAP_CAP);
    assert!(map.contains("id-0"));

    map.insert("id-overflow".to_string(), ToolCallDisplayState::default());
    assert_eq!(map.len(), TOOL_CALL_MAP_CAP);
    assert!(!map.contains("id-0"), "oldest entry should be evicted");
    assert!(map.contains("id-overflow"));
    assert!(map.contains(&format!("id-{}", TOOL_CALL_MAP_CAP - 1)));
}

#[test]
fn tool_call_map_overwrite_on_id_reuse_replaces_state_and_refreshes_position() {
    let mut map = ToolCallMap::new();
    let first = ToolCallDisplayState {
        title: Some("first".to_string()),
        ..ToolCallDisplayState::default()
    };
    map.insert("id-a".to_string(), first);
    map.insert("id-b".to_string(), ToolCallDisplayState::default());

    let replacement = ToolCallDisplayState {
        title: Some("second".to_string()),
        ..ToolCallDisplayState::default()
    };
    map.insert("id-a".to_string(), replacement);

    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get("id-a").and_then(|s| s.title.clone()).as_deref(),
        Some("second")
    );

    // Reused id moves to the most-recent FIFO slot. Filling to the cap
    // and inserting one more should evict id-b before id-a.
    for i in 0..(TOOL_CALL_MAP_CAP - 2) {
        map.insert(format!("id-fill-{i}"), ToolCallDisplayState::default());
    }
    assert_eq!(map.len(), TOOL_CALL_MAP_CAP);
    map.insert("id-overflow".to_string(), ToolCallDisplayState::default());
    assert!(!map.contains("id-b"), "id-b should be evicted before id-a");
    assert!(map.contains("id-a"));
}

#[test]
fn tool_call_map_merge_returns_none_for_missing_entry() {
    let mut map = ToolCallMap::new();
    let payload = ToolCallPayload::from_value(&json!({ "status": "completed" }));
    assert!(map.merge("nope", &payload).is_none());
}

#[test]
fn tool_call_map_merge_applies_to_existing_entry() {
    let mut map = ToolCallMap::new();
    let initial = state_from_payload(&json!({
        "kind": "read",
        "title": "Read file",
    }));
    map.insert("id-x".to_string(), initial);

    let update = ToolCallPayload::from_value(&json!({
        "status": "completed",
        "rawOutput": { "exit_code": 0, "stdout": "ok" }
    }));
    let merged = map.merge("id-x", &update).expect("entry exists");
    assert_eq!(merged.kind.as_deref(), Some("read"));
    assert_eq!(merged.status.as_deref(), Some("completed"));
    assert_eq!(merged.title.as_deref(), Some("Read file"));
}

#[test]
fn tool_call_map_evict_removes_entry_and_clears_order() {
    let mut map = ToolCallMap::new();
    map.insert("id-x".to_string(), ToolCallDisplayState::default());
    map.insert("id-y".to_string(), ToolCallDisplayState::default());
    map.evict("id-x");
    assert!(!map.contains("id-x"));
    assert_eq!(map.len(), 1);

    // Re-inserting the same id should not collide with stale order entries.
    map.insert("id-x".to_string(), ToolCallDisplayState::default());
    assert_eq!(map.len(), 2);
}

#[test]
fn tool_call_map_activity_dedup_markers_are_unbounded_per_session() {
    // Activity dedup must remain monotonic for the entire managed-ACP
    // session, regardless of how many distinct ids the agent emits. A
    // bounded FIFO would let an early id's marker age out and re-arm
    // Start/Finish for that id later in the run, breaking the watchdog
    // contract that each id emits at most one Start and one Finish.
    let mut map = ToolCallMap::new();
    let overflow_count = TOOL_CALL_MAP_CAP * 4;
    for i in 0..overflow_count {
        let id = format!("id-{i}");
        map.mark_start_emitted(&id);
        map.mark_terminal_emitted(&id);
    }
    for i in 0..overflow_count {
        let id = format!("id-{i}");
        assert!(map.start_emitted(&id), "start marker missing for {id}");
        assert!(map.terminal_emitted(&id), "finish marker missing for {id}");
    }
}

#[test]
fn tool_call_map_insert_preserves_watchdog_dedup_markers() {
    // Display-side `entries` may overwrite on id reuse, but the watchdog
    // Start/Finish markers must remain monotonic for the lifetime of the
    // session so a second `tool_call` payload for an already-tracked id
    // cannot resurrect either transition.
    let mut map = ToolCallMap::new();
    map.mark_start_emitted("id-x");
    map.mark_terminal_emitted("id-x");
    assert!(map.start_emitted("id-x"));
    assert!(map.terminal_emitted("id-x"));

    map.insert("id-x".to_string(), ToolCallDisplayState::default());
    assert!(map.start_emitted("id-x"), "start dedup must survive insert");
    assert!(
        map.terminal_emitted("id-x"),
        "finish dedup must survive insert"
    );
}

#[test]
fn terminal_status_set_matches_spec() {
    for status in [
        "completed",
        "failed",
        "cancelled",
        "canceled",
        "errored",
        "error",
    ] {
        assert!(is_terminal_status(status), "{status} should be terminal");
    }
    for status in ["in_progress", "pending", ""] {
        assert!(
            !is_terminal_status(status),
            "{status} should not be terminal"
        );
    }
}
