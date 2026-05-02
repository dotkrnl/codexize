//! Pure parsers and formatters for ACP `tool_call` / `tool_call_update`
//! payloads, plus the per-session merge map that turns those wire messages
//! into the two-block invocation/result transcript contract.

use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};

pub(super) const SNIPPET_MAX_CHARS: usize = 160;
pub(super) const INVOCATION_LINE_MAX: usize = 200;
pub(super) const RESULT_LINE_MAX: usize = 200;
pub(super) const INVOCATION_PREFIX: &str = "tool: ";
pub(super) const RESULT_PREFIX: &str = "result: ";

/// Hard ceiling on tracked tool-call entries per session. Defends against
/// pathological agents that emit an unbounded stream of `tool_call`s without
/// terminal updates.
pub(super) const TOOL_CALL_MAP_CAP: usize = 256;

const TRUNCATE_SUFFIX: &str = "...";

#[derive(Debug, Default, Clone)]
pub(super) struct ToolCallPayload {
    pub(super) tool_call_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) status: Option<String>,
    pub(super) locations: Vec<PathBuf>,
    pub(super) raw_input: Value,
    pub(super) raw_output: Value,
    pub(super) content: Vec<Value>,
}

impl ToolCallPayload {
    pub(super) fn from_value(value: &Value) -> Self {
        let non_empty_string = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|text| !text.is_empty())
        };
        let locations = value
            .get("locations")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("path").and_then(Value::as_str))
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Self {
            tool_call_id: non_empty_string("toolCallId"),
            title: non_empty_string("title"),
            kind: non_empty_string("kind"),
            status: non_empty_string("status"),
            locations,
            raw_input: value.get("rawInput").cloned().unwrap_or(Value::Null),
            raw_output: value.get("rawOutput").cloned().unwrap_or(Value::Null),
            content,
        }
    }
}

/// Merged view of one tool call across `tool_call` / `tool_call_update`
/// messages. Distinct from [`ToolCallPayload`], which is a snapshot of a
/// single wire message.
#[derive(Debug, Default, Clone)]
pub(super) struct ToolCallDisplayState {
    pub(super) title: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) status: Option<String>,
    pub(super) locations: Vec<PathBuf>,
    pub(super) raw_input: Value,
    pub(super) raw_output: Value,
    pub(super) content: Vec<Value>,
}

impl ToolCallDisplayState {
    pub(super) fn from_payload(payload: &ToolCallPayload) -> Self {
        Self {
            title: payload.title.clone(),
            kind: payload.kind.clone(),
            status: payload.status.clone(),
            locations: payload.locations.clone(),
            raw_input: payload.raw_input.clone(),
            raw_output: payload.raw_output.clone(),
            content: payload.content.clone(),
        }
    }

    /// Merge non-empty fields from `payload`. Absent / null values never erase
    /// previously-known state per spec §State Lifecycle.
    pub(super) fn merge(&mut self, payload: &ToolCallPayload) {
        if payload.title.is_some() {
            self.title = payload.title.clone();
        }
        if payload.kind.is_some() {
            self.kind = payload.kind.clone();
        }
        if payload.status.is_some() {
            self.status = payload.status.clone();
        }
        if !payload.locations.is_empty() {
            self.locations = payload.locations.clone();
        }
        if !payload.raw_input.is_null() {
            self.raw_input = payload.raw_input.clone();
        }
        if !payload.raw_output.is_null() {
            self.raw_output = payload.raw_output.clone();
        }
        if !payload.content.is_empty() {
            self.content = payload.content.clone();
        }
    }
}

/// Per-session map of `toolCallId` → merged display state, with FIFO-bounded
/// capacity and overwrite-on-id-reuse semantics per spec §State Lifecycle.
#[derive(Debug, Default)]
pub(super) struct ToolCallMap {
    entries: BTreeMap<String, ToolCallDisplayState>,
    insertion_order: VecDeque<String>,
}

impl ToolCallMap {
    pub(super) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn get(&self, id: &str) -> Option<&ToolCallDisplayState> {
        self.entries.get(id)
    }

    /// Insert or replace state for `id`. A reused id is treated as a fresh
    /// invocation: the previous entry is dropped and the new one takes the
    /// most-recent FIFO slot. When the map is at the 256-entry cap, the
    /// oldest entry is dropped first.
    pub(super) fn insert(&mut self, id: String, state: ToolCallDisplayState) {
        if self.entries.remove(&id).is_some() {
            self.insertion_order.retain(|existing| existing != &id);
        }
        while self.insertion_order.len() >= TOOL_CALL_MAP_CAP {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.insertion_order.push_back(id.clone());
        self.entries.insert(id, state);
    }

    /// Merge non-empty fields from `payload` into the existing entry for
    /// `id`. Returns the merged state, or `None` when no entry exists.
    pub(super) fn merge(
        &mut self,
        id: &str,
        payload: &ToolCallPayload,
    ) -> Option<&ToolCallDisplayState> {
        let entry = self.entries.get_mut(id)?;
        entry.merge(payload);
        Some(entry)
    }

    pub(super) fn evict(&mut self, id: &str) {
        if self.entries.remove(id).is_some() {
            self.insertion_order.retain(|existing| existing != id);
        }
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
}

pub(super) fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "canceled" | "errored" | "error"
    )
}

fn is_success_status(status: &str) -> bool {
    matches!(status, "completed" | "success" | "succeeded" | "ok")
}

/// Sanitize a raw output snippet per spec §Result Formatting:
/// 1. Strip ANSI CSI / OSC escapes and bare `ESC X` sequences.
/// 2. Replace control chars (except `\t`, `\n`, `\r`) and `0x7F` with a space.
/// 3. Replace tabs / newlines / runs of whitespace with a single space; trim.
///
/// Truncation is intentionally *not* applied here.
pub(super) fn sanitize_snippet(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1B}' {
            consume_escape(&mut chars);
            continue;
        }
        let code = c as u32;
        let is_replaced_control =
            (code <= 0x1F && c != '\t' && c != '\n' && c != '\r') || code == 0x7F;
        let is_whitespace_run = matches!(c, ' ' | '\t' | '\n' | '\r') || is_replaced_control;
        if is_whitespace_run {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

fn consume_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            // CSI: parameter / intermediate bytes followed by a final byte 0x40..=0x7E.
            for next in chars.by_ref() {
                if matches!(next, '\u{40}'..='\u{7E}') {
                    break;
                }
            }
        }
        Some(']') => {
            chars.next();
            // OSC terminated by BEL or ST (ESC \).
            while let Some(&next) = chars.peek() {
                if next == '\u{07}' {
                    chars.next();
                    return;
                }
                if next == '\u{1B}' {
                    chars.next();
                    if chars.peek().copied() == Some('\\') {
                        chars.next();
                    }
                    return;
                }
                chars.next();
            }
        }
        Some(_) => {
            // Bare ESC X: drop the single following byte.
            chars.next();
        }
        None => {}
    }
}

/// Truncate `text` so its char count does not exceed `max`, appending `...`
/// if any characters were dropped. Never splits a UTF-8 code point.
pub(super) fn truncate_with_ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let suffix_len = TRUNCATE_SUFFIX.chars().count();
    let target = max.saturating_sub(suffix_len);
    let cutoff = text
        .char_indices()
        .nth(target)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let mut out = String::with_capacity(cutoff + TRUNCATE_SUFFIX.len());
    out.push_str(&text[..cutoff]);
    out.push_str(TRUNCATE_SUFFIX);
    out
}

fn collapse_line_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnippetKind {
    Output,
    Stderr,
}

fn select_snippet(state: &ToolCallDisplayState) -> Option<(SnippetKind, String)> {
    let status = state.status.as_deref();
    let success = status.map(is_success_status).unwrap_or(true);

    if let Some(map) = state.raw_output.as_object() {
        if !success && let Some(text) = map.get("stderr").and_then(Value::as_str) {
            let cleaned = sanitize_snippet(text);
            if !cleaned.is_empty() {
                return Some((SnippetKind::Stderr, cleaned));
            }
        }
        for key in ["formatted_output", "aggregated_output", "stdout"] {
            if let Some(text) = map.get(key).and_then(Value::as_str) {
                let cleaned = sanitize_snippet(text);
                if !cleaned.is_empty() {
                    return Some((SnippetKind::Output, cleaned));
                }
            }
        }
    } else if let Some(text) = state.raw_output.as_str() {
        let cleaned = sanitize_snippet(text);
        if !cleaned.is_empty() {
            return Some((SnippetKind::Output, cleaned));
        }
    }

    for block in &state.content {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            let cleaned = sanitize_snippet(text);
            if !cleaned.is_empty() {
                return Some((SnippetKind::Output, cleaned));
            }
        }
        if let Some(text) = block.pointer("/content/text").and_then(Value::as_str) {
            let cleaned = sanitize_snippet(text);
            if !cleaned.is_empty() {
                return Some((SnippetKind::Output, cleaned));
            }
        }
    }
    None
}

fn shorten_path(path: &Path, cwd: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(cwd) {
        let rendered = rel.to_string_lossy();
        if !rendered.is_empty() {
            return rendered.into_owned();
        }
    }
    if let Some(name) = path.file_name() {
        return name.to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
}

fn extract_command(raw_input: &Value) -> Option<String> {
    if let Some(items) = raw_input.get("command").and_then(Value::as_array) {
        let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
        if !parts.is_empty() {
            // `["/bin/zsh", "-lc", "<script>"]`: the script is the meaningful payload.
            if parts.len() >= 2 && parts[parts.len() - 2] == "-lc" {
                return Some(parts[parts.len() - 1].to_string());
            }
            return Some(parts.join(" "));
        }
    }
    if let Some(text) = raw_input
        .pointer("/parsed_cmd/0/cmd")
        .and_then(Value::as_str)
        && !text.is_empty()
    {
        return Some(text.to_string());
    }
    for path in [
        "/arguments/cmd",
        "/arguments/command",
        "/cmd",
        "/command",
        "/script",
    ] {
        if let Some(text) = raw_input.pointer(path).and_then(Value::as_str)
            && !text.is_empty()
        {
            return Some(text.to_string());
        }
    }
    None
}

fn invocation_label(state: &ToolCallDisplayState, cwd: &Path) -> Option<String> {
    let kind = state.kind.as_deref();

    // Rule 1: `read` with locations.
    if kind == Some("read") && !state.locations.is_empty() {
        let head = shorten_path(&state.locations[0], cwd);
        return Some(if state.locations.len() == 1 {
            format!("read({head})")
        } else {
            format!("read({head}, +{} more)", state.locations.len() - 1)
        });
    }

    // Rules 2 & 3: `execute` kind or recognized raw-command input.
    if let Some(cmd) = extract_command(&state.raw_input) {
        return Some(format!("exec({cmd})"));
    }
    if kind == Some("execute") {
        // No command extractable; fall through to title / fallback rules.
    }

    // Rule 4: other kind with at least one location.
    if let (Some(kind), Some(loc)) = (kind, state.locations.first())
        && kind != "read"
        && kind != "execute"
    {
        let path = shorten_path(loc, cwd);
        return Some(format!("{kind}({path})"));
    }

    // Rule 5: verbatim title (sanitized only, never reshaped).
    if let Some(title) = state.title.as_deref() {
        let cleaned = sanitize_snippet(title);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }

    // Rule 6: literal `tool` handled by the caller.
    None
}

pub(super) fn format_invocation_line(state: &ToolCallDisplayState, cwd: &Path) -> String {
    let body = invocation_label(state, cwd).unwrap_or_else(|| "tool".to_string());
    let assembled = format!("{INVOCATION_PREFIX}{body}");
    let collapsed = collapse_line_whitespace(&assembled);
    truncate_with_ellipsis(&collapsed, INVOCATION_LINE_MAX)
}

pub(super) fn format_result_line(state: &ToolCallDisplayState) -> String {
    let status = state
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let mut line = format!("{RESULT_PREFIX}{status}");

    if let Some(exit) = state
        .raw_output
        .get("exit_code")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
    {
        line.push_str(&format!(", exit {exit}"));
    }

    if let Some((kind, text)) = select_snippet(state) {
        let snippet = truncate_with_ellipsis(&text, SNIPPET_MAX_CHARS);
        let label = match kind {
            SnippetKind::Stderr => "stderr",
            SnippetKind::Output => "output",
        };
        line.push_str(&format!(", {label}: {snippet}"));
    }

    let collapsed = collapse_line_whitespace(&line);
    truncate_with_ellipsis(&collapsed, RESULT_LINE_MAX)
}

#[cfg(test)]
mod tests {
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
}
