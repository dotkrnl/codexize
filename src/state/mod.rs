pub mod phase;
pub mod transitions;
pub mod resume;

pub use phase::Phase;
pub use transitions::execute_transition;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// An event logged to the run's events.jsonl audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub timestamp: String,
    pub phase: Phase,
    pub message: String,
}

/// Model selected for a specific phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseModel {
    pub model: String,
    pub vendor: String,
}

/// Tracks the builder loop — which tasks are pending, done, what iteration
/// we're on, and enough state to resume a killed session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuilderState {
    /// Task IDs still to do, in order.
    #[serde(default)]
    pub pending: Vec<u32>,
    /// Task IDs already accepted (reviewer said "done").
    #[serde(default)]
    pub done: Vec<u32>,
    /// The task being worked on right now (None between rounds or at end).
    #[serde(default)]
    pub current_task: Option<u32>,
    /// Global iteration counter — one coder+reviewer cycle is one iteration.
    #[serde(default)]
    pub iteration: u32,
    /// True if we've launched the coder for the current iteration at least
    /// once — subsequent launches use the CLI's --continue flag to resume
    /// the session rather than start fresh.
    #[serde(default)]
    pub coder_started: bool,
    /// Same, for the reviewer.
    #[serde(default)]
    pub reviewer_started: bool,
    /// Last reviewer verdict status ("done", "revise", "blocked") — used to
    /// decide where to resume on restart.
    #[serde(default)]
    pub last_verdict: Option<String>,
}

/// The persisted state of a single codexize session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: String,
    pub current_phase: Phase,
    #[serde(default)]
    pub idea_text: Option<String>,
    #[serde(default)]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub agent_error: Option<String>,
    /// Model used per phase, keyed by phase label string
    #[serde(default)]
    pub phase_models: std::collections::BTreeMap<String, PhaseModel>,
    /// All spec reviewers in order (may be multiple rounds)
    #[serde(default)]
    pub spec_reviewers: Vec<PhaseModel>,
    /// Builder loop state (empty until sharding completes)
    #[serde(default)]
    pub builder: BuilderState,
    #[serde(default)]
    pub archived: bool,
}

impl SessionState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            current_phase: Phase::IdeaInput,
            idea_text: None,
            selected_model: None,
            agent_error: None,
            phase_models: std::collections::BTreeMap::new(),
            spec_reviewers: Vec::new(),
            builder: BuilderState::default(),
            archived: false,
        }
    }

    pub fn load(session_id: &str) -> Result<Self> {
        let path = session_dir(session_id).join("session.toml");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read session state from {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse session state from {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create session directory {}", dir.display()))?;
        let path = dir.join("session.toml");
        let text = toml::to_string_pretty(self).context("failed to serialize session state")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write session state to {}", path.display()))?;
        Ok(())
    }

    /// Append an event to the session's events.jsonl audit trail.
    pub fn log_event(&self, message: impl Into<String>) -> Result<()> {
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join("events.jsonl");

        let event = Event {
            timestamp: chrono::Utc::now().to_rfc3339(),
            phase: self.current_phase,
            message: message.into(),
        };

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let line = serde_json::to_string(&event).context("failed to serialize event")?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Transition to a new phase with validation and persistence.
    pub fn transition_to(&mut self, next_phase: Phase) -> Result<()> {
        execute_transition(self, next_phase)
    }
}

/// Return the directory path for a given session ID.
pub fn session_dir(session_id: &str) -> PathBuf {
    Path::new(".codexize").join("sessions").join(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_state_archived_defaults_false() {
        let state = SessionState::new("test-session".to_string());
        assert!(!state.archived);
    }

    #[test]
    fn test_session_state_archived_persists() {
        let mut state = SessionState::new("test-session".to_string());
        state.archived = true;

        let toml = toml::to_string(&state).unwrap();
        assert!(toml.contains("archived = true"));

        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert!(loaded.archived);
    }

    #[test]
    fn test_session_state_archived_defaults_false_on_deserialize() {
        let state = SessionState::new("test-session".to_string());
        let toml = toml::to_string(&state).unwrap();
        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert!(!loaded.archived);
    }
}
