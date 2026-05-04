use crate::{
    picker::SessionEntry,
    state::{self, SessionState},
};
use anyhow::Result;
use std::fs;

pub fn scan_sessions() -> Result<Vec<SessionEntry>> {
    let sessions_dir = state::codexize_root().join("sessions");

    if !sessions_dir.exists() {
        fs::create_dir_all(&sessions_dir)?;
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();

    for entry in fs::read_dir(&sessions_dir)? {
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
                .map(str::to_string)
                .unwrap_or_else(|| truncate_idea(&state.idea_text)),
            current_phase: state.current_phase,
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

fn truncate_idea(idea: &Option<String>) -> String {
    match idea {
        Some(text) if text.chars().count() > 80 => {
            format!("{}...", text.chars().take(80).collect::<String>())
        }
        Some(text) => text.clone(),
        None => "(no idea yet)".to_string(),
    }
}
