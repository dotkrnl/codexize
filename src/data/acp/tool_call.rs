//! Pure parsers and formatters for ACP `tool_call` / `tool_call_update`
//! payloads, plus the per-session merge map.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

pub(super) const SNIPPET_MAX_CHARS: usize = 160;
pub(super) const INVOCATION_LINE_MAX: usize = 200;
pub(super) const RESULT_LINE_MAX: usize = 200;
pub(super) const INVOCATION_PREFIX: &str = "tool: ";
pub(super) const RESULT_PREFIX: &str = "result: ";
pub(super) const TOOL_CALL_MAP_CAP: usize = 256;

#[derive(Debug, Default, Clone)]
pub(super) struct ToolCallDisplayState {
    pub(super) tool_call_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) status: Option<String>,
    pub(super) locations: Vec<PathBuf>,
    pub(super) raw_input: Value,
    pub(super) raw_output: Value,
    pub(super) content: Vec<Value>,
}

fn nonempty_str(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

impl ToolCallDisplayState {
    #[rustfmt::skip]
    pub(super) fn from_value(v: &Value) -> Self {
        Self {
            tool_call_id: nonempty_str(v, "toolCallId"),
            title: nonempty_str(v, "title"),
            kind: nonempty_str(v, "kind"),
            status: nonempty_str(v, "status"),
            locations: v.get("locations").and_then(Value::as_array).map(|items| {
                items.iter().filter_map(|i| i.get("path").and_then(Value::as_str))
                    .filter(|p| !p.is_empty()).map(PathBuf::from).collect()
            }).unwrap_or_default(),
            raw_input: v.get("rawInput").cloned().unwrap_or(Value::Null),
            raw_output: v.get("rawOutput").cloned().unwrap_or(Value::Null),
            content: v.get("content").and_then(Value::as_array).cloned().unwrap_or_default(),
        }
    }

    /// Merge non-empty fields; absent / null values never erase known state.
    #[rustfmt::skip]
    pub(super) fn merge(&mut self, p: &ToolCallDisplayState) {
        if p.title.is_some() { self.title = p.title.clone(); }
        if p.kind.is_some() { self.kind = p.kind.clone(); }
        if p.status.is_some() { self.status = p.status.clone(); }
        if !p.locations.is_empty() { self.locations = p.locations.clone(); }
        if !p.raw_input.is_null() { self.raw_input = p.raw_input.clone(); }
        if !p.raw_output.is_null() { self.raw_output = p.raw_output.clone(); }
        if !p.content.is_empty() { self.content = p.content.clone(); }
    }
}

/// `toolCallId` → merged display state. FIFO-bounded with overwrite-on-id-reuse;
/// `emitted` records (id, is_terminal) monotonically.
#[derive(Debug, Default)]
pub(super) struct ToolCallMap {
    entries: BTreeMap<String, ToolCallDisplayState>,
    order: VecDeque<String>,
    emitted: BTreeSet<(String, bool)>,
}

#[rustfmt::skip]
impl ToolCallMap {
    pub(super) fn new() -> Self { Self::default() }

    #[cfg(test)] pub(super) fn len(&self) -> usize { self.entries.len() }
    #[cfg(test)] pub(super) fn get(&self, id: &str) -> Option<&ToolCallDisplayState> { self.entries.get(id) }
    #[cfg(test)] pub(super) fn contains(&self, id: &str) -> bool { self.entries.contains_key(id) }

    pub(super) fn insert(&mut self, id: String, state: ToolCallDisplayState) {
        if self.entries.remove(&id).is_some() { self.order.retain(|x| x != &id); }
        while self.order.len() >= TOOL_CALL_MAP_CAP {
            let Some(o) = self.order.pop_front() else { break };
            self.entries.remove(&o);
        }
        self.order.push_back(id.clone());
        self.entries.insert(id, state);
    }

    pub(super) fn merge(&mut self, id: &str, p: &ToolCallDisplayState) -> Option<&ToolCallDisplayState> {
        let entry = self.entries.get_mut(id)?;
        entry.merge(p);
        Some(entry)
    }

    pub(super) fn evict(&mut self, id: &str) {
        if self.entries.remove(id).is_some() { self.order.retain(|x| x != id); }
    }

    pub(super) fn mark_emitted(&mut self, id: &str, terminal: bool) {
        self.emitted.insert((id.to_string(), terminal));
    }
    pub(super) fn was_emitted(&self, id: &str, terminal: bool) -> bool {
        self.emitted.contains(&(id.to_string(), terminal))
    }
}

pub(super) fn is_terminal_status(s: &str) -> bool {
    matches!(
        s,
        "completed" | "failed" | "cancelled" | "canceled" | "errored" | "error"
    )
}

fn is_success_status(s: &str) -> bool {
    matches!(s, "completed" | "success" | "succeeded" | "ok")
}

/// Strip ANSI escapes; replace control chars and whitespace runs with one space.
#[rustfmt::skip]
pub(super) fn sanitize_snippet(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in strip_ansi_escapes::strip_str(input).chars() {
        let code = c as u32;
        let is_ctrl = (code <= 0x1F && c != '\t' && c != '\n' && c != '\r') || code == 0x7F;
        if c.is_ascii_whitespace() || is_ctrl {
            if !out.ends_with(' ') { out.push(' '); }
        } else { out.push(c); }
    }
    out.trim().to_string()
}

/// Truncate to `max` chars, appending `...` if any chars dropped.
#[rustfmt::skip]
pub(super) fn truncate_with_ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() <= max { return text.to_string(); }
    let cutoff = text.char_indices().nth(max.saturating_sub(3)).map(|(i, _)| i).unwrap_or(text.len());
    format!("{}...", &text[..cutoff])
}

fn collapse_ws(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnippetKind {
    Output,
    Stderr,
}

#[rustfmt::skip]
fn select_snippet(state: &ToolCallDisplayState) -> Option<(SnippetKind, String)> {
    let success = state.status.as_deref().map(is_success_status).unwrap_or(true);
    let pick = |k, t: Option<&str>| {
        let s = sanitize_snippet(t?);
        (!s.is_empty()).then_some((k, s))
    };
    if let Some(map) = state.raw_output.as_object() {
        if !success
            && let hit @ Some(_) = pick(SnippetKind::Stderr, map.get("stderr").and_then(Value::as_str))
        { return hit; }
        for key in ["formatted_output", "aggregated_output", "stdout"] {
            if let hit @ Some(_) = pick(SnippetKind::Output, map.get(key).and_then(Value::as_str)) {
                return hit;
            }
        }
    } else if let hit @ Some(_) = pick(SnippetKind::Output, state.raw_output.as_str()) {
        return hit;
    }
    for block in &state.content {
        for ptr in ["/text", "/content/text"] {
            if let hit @ Some(_) = pick(SnippetKind::Output, block.pointer(ptr).and_then(Value::as_str)) {
                return hit;
            }
        }
    }
    None
}

#[rustfmt::skip]
fn shorten_path(path: &Path, cwd: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(cwd) && !rel.as_os_str().is_empty() {
        return rel.to_string_lossy().into_owned();
    }
    path.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[rustfmt::skip]
fn extract_command(raw: &Value) -> Option<String> {
    if let Some(items) = raw.get("command").and_then(Value::as_array) {
        let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
        if !parts.is_empty() {
            // `["/bin/zsh", "-lc", "<script>"]`: keep script payload only.
            if parts.len() >= 2 && parts[parts.len() - 2] == "-lc" {
                return Some(parts[parts.len() - 1].to_string());
            }
            return Some(parts.join(" "));
        }
    }
    ["/parsed_cmd/0/cmd", "/arguments/cmd", "/arguments/command", "/cmd", "/command", "/script"]
        .iter()
        .find_map(|p| raw.pointer(p).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string))
}

#[rustfmt::skip]
fn invocation_label(state: &ToolCallDisplayState, cwd: &Path) -> Option<String> {
    let kind = state.kind.as_deref();
    if kind == Some("read") && !state.locations.is_empty() {
        let head = shorten_path(&state.locations[0], cwd);
        let rest = state.locations.len() - 1;
        return Some(if rest == 0 { format!("read({head})") } else { format!("read({head}, +{rest} more)") });
    }
    if let Some(cmd) = extract_command(&state.raw_input) { return Some(format!("exec({cmd})")); }
    if let (Some(k), Some(loc)) = (kind, state.locations.first()) && !matches!(k, "read" | "execute") {
        return Some(format!("{k}({})", shorten_path(loc, cwd)));
    }
    let title = sanitize_snippet(state.title.as_deref()?);
    (!title.is_empty()).then_some(title)
}

pub(super) fn format_invocation_line(state: &ToolCallDisplayState, cwd: &Path) -> String {
    let body = invocation_label(state, cwd).unwrap_or_else(|| "tool".to_string());
    truncate_with_ellipsis(
        &collapse_ws(&format!("{INVOCATION_PREFIX}{body}")),
        INVOCATION_LINE_MAX,
    )
}

#[rustfmt::skip]
pub(super) fn format_result_line(state: &ToolCallDisplayState) -> String {
    let status = state.status.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("unknown");
    let mut line = format!("{RESULT_PREFIX}{status}");
    if let Some(exit) = state.raw_output.get("exit_code")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
    {
        line.push_str(&format!(", exit {exit}"));
    }
    if let Some((kind, text)) = select_snippet(state) {
        let snippet = truncate_with_ellipsis(&text, SNIPPET_MAX_CHARS);
        let label = match kind { SnippetKind::Stderr => "stderr", SnippetKind::Output => "output" };
        line.push_str(&format!(", {label}: {snippet}"));
    }
    truncate_with_ellipsis(&collapse_ws(&line), RESULT_LINE_MAX)
}

#[cfg(test)]
#[path = "tool_call_tests.rs"]
mod tests_mod;
