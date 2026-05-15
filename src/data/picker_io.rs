use crate::{
    data::config::Config,
    data::session_index,
    scheduler::ScannedSession,
    state::{self, SessionState, Stage},
    ui::widgets::picker::state::SessionEntry,
};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

pub fn default_sessions_root() -> PathBuf {
    state::codexize_root().join("sessions")
}

/// Resolve the sessions directory from a loaded [`Config`]: honor an explicit
/// `paths.sessions_root` override, otherwise use the project-local default.
pub fn sessions_root_for(config: &Config) -> PathBuf {
    if config.paths.sessions_root.is_explicit() {
        config.paths_view().sessions_root
    } else {
        default_sessions_root()
    }
}

/// Scan non-archived sessions and return them sorted by session-id creation
/// order (ascending). The session-id timestamp format makes this a simple
/// lexicographic sort.
pub fn scan_sessions_by_creation_order(sessions_dir: &Path) -> Result<Vec<SessionEntry>> {
    let mut entries = scan_sessions(sessions_dir)?;
    entries.retain(|e| !e.archived);
    entries.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Ok(entries)
}

/// Compute the "newest earlier Done baseline" for `session_id` from a list of
/// sessions sorted by creation order.
///
/// Definition (spec § Data model / Session fields): the newest non-archived
/// session whose session id sorts earlier than `session_id` and whose stage is
/// `Done`.
pub fn newest_earlier_done_baseline(session_id: &str, sessions: &[SessionEntry]) -> Option<String> {
    sessions
        .iter()
        .filter(|e| e.session_id.as_str() < session_id && e.current_stage == Stage::Done)
        .map(|e| e.session_id.clone())
        .next_back()
}
/// Scan non-archived sessions for the queue scheduler.
///
/// Returns entries sorted by session-id creation order (ascending) with
/// per-session load failures surfaced as `ScannedSession::Corrupt` rather
/// than dropped silently. Archived sessions are still excluded — they are
/// invisible to the scheduler. The caller (the shell) decides whether a
/// corrupt entry blocks the implementation lane (only earlier-than-head
/// failures do, per spec).
pub fn scan_sessions_for_scheduler(sessions_dir: &Path) -> Result<Vec<ScannedSession>> {
    // One-shot SessionIndex snapshot. Long-lived callers (the shell
    // scheduler tick) hold a SessionIndex directly so they pay only for
    // entries whose `session.toml` mtime advanced.
    session_index::snapshot_for_scheduler(sessions_dir)
}

pub fn scan_sessions(sessions_dir: &Path) -> Result<Vec<SessionEntry>> {
    if !sessions_dir.exists() {
        fs::create_dir_all(sessions_dir)?;
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let session_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let toml_path = path.join("session.toml");
        if !toml_path.exists() {
            continue;
        }
        let state = match SessionState::load(&session_id) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let last_modified = fs::metadata(&toml_path)?.modified()?;
        entries.push(SessionEntry {
            session_id,
            idea_summary: state
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map_or_else(|| truncate_idea(&state.idea_text), str::to_string),
            current_stage: state.current_stage,
            modes: state.modes,
            last_modified,
            archived: state.archived,
        });
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_modified));
    Ok(entries)
}
pub fn delete_session(session_id: &str) -> Result<()> {
    fs::remove_dir_all(state::session_dir(session_id))?;
    Ok(())
}
/// Shared sidebar/picker title text for a session whose title is empty.
///
/// Lives here (the original home of the picker's `idea_summary` formatting)
/// so the session-index sidebar projection in
/// [`crate::data::session_index::indexed_session_from_state`] produces
/// character-identical output for the same input. Any tweak to the
/// truncation width or the "(no idea yet)" wording is therefore intentional
/// and visible to both call sites at once.
pub(crate) fn truncate_idea(idea: &Option<String>) -> String {
    match idea {
        Some(text) if text.chars().count() > 80 => {
            format!("{}...", text.chars().take(80).collect::<String>())
        }
        Some(text) => text.clone(),
        None => "(no idea yet)".to_string(),
    }
}
