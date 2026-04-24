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
    pub schema_version: u32,
    #[serde(default)]
    pub agent_runs: Vec<RunRecord>,
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
            schema_version: 2,
            agent_runs: Vec::new(),
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

        // Reject v1 files (no schema_version field)
        let raw: toml::Value = toml::from_str(&text)
            .with_context(|| format!("failed to parse session state from {}", path.display()))?;
        if raw.get("schema_version").is_none() {
            anyhow::bail!(
                "session {} uses schema v1; archive with `codexize archive {}` and start fresh.",
                session_id, session_id
            );
        }

        let state: SessionState = toml::from_str(&text)
            .with_context(|| format!("failed to parse session state from {}", path.display()))?;

        if state.schema_version != 2 {
            anyhow::bail!(
                "session {} uses schema v{}; archive with `codexize archive {}` and start fresh.",
                session_id,
                state.schema_version,
                session_id
            );
        }

        Ok(state)
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

    /// Append a message to the session's messages.jsonl file.
    pub fn append_message(&self, message: &Message) -> Result<()> {
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join("messages.jsonl");

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let line = serde_json::to_string(message)
            .context("failed to serialize message")?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Load all messages for a session from messages.jsonl.
    pub fn load_messages(session_id: &str) -> Result<Vec<Message>> {
        let path = session_dir(session_id).join("messages.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read messages from {}", path.display()))?;

        let mut messages = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    // Skip corrupt line, log to stderr (best effort)
                    eprintln!("WARNING: corrupt message line {} in {}: {}", line_num + 1, session_id, e);
                }
            }
        }

        Ok(messages)
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
    fn test_session_state_schema_v2() {
        use tempfile::TempDir;
        use std::env;

        let temp = TempDir::new().unwrap();
        unsafe {
            env::set_var("CODEXIZE_ROOT", temp.path());
        }

        let mut state = SessionState::new("test-session".to_string());
        state.schema_version = 2;
        state.agent_runs.push(RunRecord {
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
        });

        state.save().unwrap();
        let loaded = SessionState::load("test-session").unwrap();

        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.agent_runs.len(), 1);
        assert_eq!(loaded.agent_runs[0].id, 1);
    }

    #[test]
    fn test_session_state_v1_rejection() {
        use tempfile::TempDir;
        use std::env;

        let temp = TempDir::new().unwrap();
        unsafe {
            env::set_var("CODEXIZE_ROOT", temp.path());
        }

        // Manually write a v1 session file (no schema_version field)
        let dir = session_dir("test-v1-session");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        std::fs::write(&path, r#"
session_id = "test-v1-session"
current_phase = "IdeaInput"
"#).unwrap();

        let result = SessionState::load("test-v1-session");
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("schema v1") || err_msg.contains("archive"));
    }

    #[test]
    fn test_append_message() {
        use tempfile::TempDir;
        use std::env;

        let temp = TempDir::new().unwrap();
        unsafe {
            env::set_var("CODEXIZE_ROOT", temp.path());
        }

        let state = SessionState::new("test-msg-session".to_string());
        state.save().unwrap();

        let msg = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::Brief,
            text: "Exploring code".to_string(),
        };

        state.append_message(&msg).unwrap();

        // Verify file exists and contains the message
        let path = session_dir("test-msg-session").join("messages.jsonl");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Exploring code"));
    }

    #[test]
    fn test_load_messages() {
        use tempfile::TempDir;
        use std::env;

        let temp = TempDir::new().unwrap();
        unsafe {
            env::set_var("CODEXIZE_ROOT", temp.path());
        }

        let state = SessionState::new("test-load-msg".to_string());
        state.save().unwrap();

        let msg1 = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::Brief,
            text: "First".to_string(),
        };
        let msg2 = Message {
            ts: chrono::Utc::now(),
            run_id: 1,
            kind: MessageKind::End,
            text: "done in 1m".to_string(),
        };

        state.append_message(&msg1).unwrap();
        state.append_message(&msg2).unwrap();

        let loaded = SessionState::load_messages("test-load-msg").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text, "First");
        assert_eq!(loaded[1].text, "done in 1m");
    }

    #[test]
    fn test_load_messages_with_corrupt_line() {
        use tempfile::TempDir;
        use std::env;

        let temp = TempDir::new().unwrap();
        unsafe {
            env::set_var("CODEXIZE_ROOT", temp.path());
        }

        let state = SessionState::new("test-corrupt-msg".to_string());
        state.save().unwrap();

        // Manually write messages.jsonl with one corrupt line
        let dir = session_dir("test-corrupt-msg");
        let path = dir.join("messages.jsonl");
        std::fs::write(&path, r#"{"ts":"2026-04-24T00:00:00Z","run_id":1,"kind":"Brief","text":"Good"}
{corrupt json line here}
{"ts":"2026-04-24T00:01:00Z","run_id":1,"kind":"End","text":"done"}
"#).unwrap();

        let loaded = SessionState::load_messages("test-corrupt-msg").unwrap();
        assert_eq!(loaded.len(), 2); // Corrupt line skipped
        assert_eq!(loaded[0].text, "Good");
        assert_eq!(loaded[1].text, "done");
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
