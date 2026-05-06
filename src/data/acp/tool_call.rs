//! Pure parsers and formatters for ACP `tool_call` / `tool_call_update`
//! payloads, plus the per-session merge map that turns those wire messages
//! into the two-block invocation/result transcript contract.

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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
    // Watchdog Start/Finish dedup is monotonic for the lifetime of the session
    // (a session corresponds to one managed-ACP run). Once a Start has been
    // emitted for an id, no later payload — including a repeated `tool_call`
    // that re-creates the display entry under the same id — may emit another
    // Start. Same one-shot rule applies to Finish. This intentionally diverges
    // from the display-side `entries` map, which is allowed to overwrite on
    // id reuse: the activity stream feeds a per-run state machine that must
    // never double-count pause windows or pause-resume transitions.
    //
    // Storage is an unbounded `BTreeSet<String>` rather than the FIFO+cap
    // discipline used for `entries`. The cap on `entries` exists to bound
    // display-state memory for pathological agents; for activity dedup an
    // FIFO eviction policy would silently re-arm Start/Finish once an old id
    // ages out, breaking the one-shot watchdog contract. We pay the (small)
    // unbounded-id-string cost — bounded by the count of distinct tool-call
    // ids the agent emits during one run — to preserve correctness.
    start_emitted: BTreeSet<String>,
    terminal_emitted: BTreeSet<String>,
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

    /// Insert or replace display state for `id`. A reused id is treated as a
    /// fresh invocation for display purposes: the previous entry is dropped
    /// and the new one takes the most-recent FIFO slot. When the map is at
    /// the 256-entry cap, the oldest entry is dropped first.
    ///
    /// Watchdog `start_emitted` / `terminal_emitted` markers are intentionally
    /// **not** cleared here; see the field comment above for why.
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

    pub(super) fn mark_start_emitted(&mut self, id: &str) {
        self.start_emitted.insert(id.to_string());
    }

    pub(super) fn start_emitted(&self, id: &str) -> bool {
        self.start_emitted.contains(id)
    }

    pub(super) fn mark_terminal_emitted(&mut self, id: &str) {
        self.terminal_emitted.insert(id.to_string());
    }

    pub(super) fn terminal_emitted(&self, id: &str) -> bool {
        self.terminal_emitted.contains(id)
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
/// 1. Strip ANSI CSI / OSC escapes and bare `ESC X` sequences (via the `vte`
///    parser inside `strip-ansi-escapes`, which handles every dispatched
///    sequence the hand-rolled state machine recognised plus the long tail
///    we did not cover).
/// 2. Replace control chars (except `\t`, `\n`, `\r`) and `0x7F` with a space.
/// 3. Replace tabs / newlines / runs of whitespace with a single space; trim.
///
/// Truncation is intentionally *not* applied here.
pub(super) fn sanitize_snippet(input: &str) -> String {
    let stripped = strip_ansi_escapes::strip_str(input);
    let mut out = String::with_capacity(stripped.len());
    for c in stripped.chars() {
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
mod tests_mod;
