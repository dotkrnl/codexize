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

/// The persisted state of a single codexize run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: String,
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
}

impl RunState {
    pub fn new(run_id: String) -> Self {
        Self {
            run_id,
            current_phase: Phase::IdeaInput,
            idea_text: None,
            selected_model: None,
            agent_error: None,
            phase_models: std::collections::BTreeMap::new(),
            spec_reviewers: Vec::new(),
            builder: BuilderState::default(),
        }
    }

    pub fn load(run_id: &str) -> Result<Self> {
        let path = run_dir(run_id).join("run.toml");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read run state from {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("failed to parse run state from {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let dir = run_dir(&self.run_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create run directory {}", dir.display()))?;
        let path = dir.join("run.toml");
        let text = toml::to_string_pretty(self).context("failed to serialize run state")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write run state to {}", path.display()))?;
        Ok(())
    }

    /// Append an event to the run's events.jsonl audit trail.
    pub fn log_event(&self, message: impl Into<String>) -> Result<()> {
        let dir = run_dir(&self.run_id);
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

/// Return the directory path for a given run ID.
pub fn run_dir(run_id: &str) -> PathBuf {
    Path::new(".codexize").join("runs").join(run_id)
}
