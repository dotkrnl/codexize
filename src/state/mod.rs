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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: u64,
    pub stage: String,
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
    pub model: String,
    pub vendor: String,
    pub window_name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: RunStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageKind {
    Brief,
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub run_id: u64,
    pub kind: MessageKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Stage,
    Task,
    Round,
    AgentRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Pending,
    Running,
    WaitingUser,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub label: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub summary: String,
    pub children: Vec<Node>,
    pub run_id: Option<u64>,
    pub leaf_run_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AttemptStatus {
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseAttempt {
    pub status: AttemptStatus,
    pub summary: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub transcript: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub live_summary: String,
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
    /// All plan reviewers in order (may be multiple rounds)
    #[serde(default)]
    pub plan_reviewers: Vec<PhaseModel>,
    /// Builder loop state (empty until sharding completes)
    #[serde(default)]
    pub builder: BuilderState,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub phase_attempts: std::collections::BTreeMap<String, Vec<PhaseAttempt>>,
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
            plan_reviewers: Vec::new(),
            builder: BuilderState::default(),
            archived: false,
            phase_attempts: std::collections::BTreeMap::new(),
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
    fn test_run_record_lifecycle_create_to_done() {
        let mut runs = Vec::new();
        let run = RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        };
        runs.push(run);

        assert_eq!(runs[0].status, RunStatus::Running);
        assert!(runs[0].ended_at.is_none());
    }

    #[test]
    fn test_run_record_transition_to_done() {
        let mut run = RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        };

        run.status = RunStatus::Done;
        run.ended_at = Some(chrono::Utc::now());

        assert_eq!(run.status, RunStatus::Done);
        assert!(run.ended_at.is_some());
        assert!(run.error.is_none());
    }

    #[test]
    fn test_run_record_transition_to_failed() {
        let mut run = RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        };

        run.status = RunStatus::Failed;
        run.ended_at = Some(chrono::Utc::now());
        run.error = Some("validation failed".to_string());

        assert_eq!(run.status, RunStatus::Failed);
        assert!(run.ended_at.is_some());
        assert_eq!(run.error.as_deref(), Some("validation failed"));
    }

    #[test]
    fn test_message_creation() {
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::Brief,
            text: "Exploring codebase".to_string(),
        };

        assert_eq!(msg.run_id, 1);
        assert_eq!(msg.kind, MessageKind::Brief);
        assert_eq!(msg.text, "Exploring codebase");
    }

    #[test]
    fn test_node_creation() {
        let node = Node {
            label: "Brainstorm".to_string(),
            kind: NodeKind::Stage,
            status: NodeStatus::Done,
            summary: "completed".to_string(),
            children: vec![],
            run_id: None,
            leaf_run_id: Some(1),
        };

        assert_eq!(node.label, "Brainstorm");
        assert_eq!(node.kind, NodeKind::Stage);
        assert_eq!(node.leaf_run_id, Some(1));
    }

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
    fn test_phase_attempts_roundtrip() {
        let mut state = SessionState::new("test".to_string());
        state.phase_attempts.insert(
            "brainstorm".to_string(),
            vec![PhaseAttempt {
                status: AttemptStatus::Failed,
                summary: "spec generation failed".to_string(),
                events: vec!["error: timeout".to_string()],
                transcript: Vec::new(),
                error: Some("timeout".to_string()),
                live_summary: "working on section 3...".to_string(),
            }],
        );
        let toml = toml::to_string(&state).unwrap();
        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert_eq!(loaded.phase_attempts.len(), 1);
        let attempts = loaded.phase_attempts.get("brainstorm").unwrap();
        assert_eq!(attempts[0].status, AttemptStatus::Failed);
        assert_eq!(attempts[0].summary, "spec generation failed");
        assert_eq!(attempts[0].live_summary, "working on section 3...");
    }

    #[test]
    fn test_session_state_archived_defaults_false_on_deserialize() {
        let state = SessionState::new("test-session".to_string());
        let toml = toml::to_string(&state).unwrap();
        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert!(!loaded.archived);
    }

    #[test]
    fn test_plan_reviewers_defaults_empty() {
        let state = SessionState::new("test".to_string());
        assert!(state.plan_reviewers.is_empty());
    }

    #[test]
    fn test_plan_reviewers_roundtrip() {
        let mut state = SessionState::new("test".to_string());
        state.plan_reviewers.push(PhaseModel {
            model: "o3".to_string(),
            vendor: "openai".to_string(),
        });
        let toml = toml::to_string(&state).unwrap();
        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert_eq!(loaded.plan_reviewers.len(), 1);
        assert_eq!(loaded.plan_reviewers[0].model, "o3");
    }
}
