pub mod phase;
pub mod resume;
pub mod transitions;

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
    Started,
    Brief,
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageSender {
    System,
    Agent { model: String, vendor: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub run_id: u64,
    pub kind: MessageKind,
    pub sender: MessageSender,
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

impl NodeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingUser => "waiting-user",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn style(self) -> ratatui::style::Style {
        use ratatui::style::{Color, Style};
        match self {
            Self::Pending => Style::default().fg(Color::DarkGray),
            Self::Running => Style::default().fg(Color::Cyan),
            Self::WaitingUser => Style::default().fg(Color::Yellow),
            Self::Done => Style::default().fg(Color::Green),
            Self::Failed => Style::default().fg(Color::Red),
        }
    }
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
    /// Last reviewer verdict status ("done", "revise", "blocked") — used to
    /// decide where to resume on restart.
    #[serde(default)]
    pub last_verdict: Option<String>,
    /// Recovery context captured when entering builder recovery.
    ///
    /// Orchestrator-owned: the recovery agent may edit artifacts, but it must not
    /// mutate queue state directly; reconciliation uses this context plus run
    /// history to enforce invariants.
    #[serde(default)]
    pub recovery_trigger_task_id: Option<u32>,
    /// Maximum task id observed before recovery began (from the pre-recovery tasks.toml).
    #[serde(default)]
    pub recovery_prev_max_task_id: Option<u32>,
    /// Full task id set observed before recovery began.
    #[serde(default)]
    pub recovery_prev_task_ids: Vec<u32>,
    /// Optional human-readable trigger summary (e.g. retry exhaustion details).
    #[serde(default)]
    pub recovery_trigger_summary: Option<String>,
    /// Builder retry reset boundary: failed coder/reviewer runs at or before this
    /// run id are ignored when rebuilding retry exclusions after restart.
    #[serde(default)]
    pub retry_reset_run_id_cutoff: Option<u64>,
    /// Short one-line titles keyed by task id, sourced from tasks.toml.
    /// Used to label task nodes in the pipeline tree.
    #[serde(default)]
    pub task_titles: std::collections::BTreeMap<u32, String>,
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
    /// Builder loop state (empty until sharding completes)
    #[serde(default)]
    pub builder: BuilderState,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub skip_to_impl_rationale: Option<String>,
    #[serde(default)]
    pub skip_to_impl_kind: Option<crate::artifacts::SkipToImplKind>,
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
            builder: BuilderState::default(),
            archived: false,
            skip_to_impl_rationale: None,
            skip_to_impl_kind: None,
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
                session_id,
                session_id
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

        let line = serde_json::to_string(message).context("failed to serialize message")?;
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
                    eprintln!(
                        "WARNING: corrupt message line {} in {}: {}",
                        line_num + 1,
                        session_id,
                        e
                    );
                }
            }
        }

        Ok(messages)
    }

    /// Create a new RunRecord, push it to agent_runs, and return its id.
    pub fn create_run_record(
        &mut self,
        stage: String,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
        model: String,
        vendor: String,
        window_name: String,
    ) -> u64 {
        let id = self.next_agent_run_id();
        let run = RunRecord {
            id,
            stage,
            task_id,
            round,
            attempt,
            model,
            vendor,
            window_name,
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        };
        self.agent_runs.push(run);
        id
    }

    /// Return the next available agent_run_id (monotonic within session).
    pub fn next_agent_run_id(&self) -> u64 {
        self.agent_runs.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }

    /// Resume running runs on session load. Returns the current run ID if exactly one Running run exists and its window is live.
    pub fn resume_running_runs(&mut self, live_windows: &[String]) -> Result<Option<u64>> {
        let running_ids: Vec<u64> = self
            .agent_runs
            .iter()
            .filter(|r| r.status == RunStatus::Running)
            .map(|r| r.id)
            .collect();

        if running_ids.is_empty() {
            return Ok(None);
        }

        if running_ids.len() > 1 {
            anyhow::bail!(
                "session {} has {} concurrent runs; repair manually by editing session.toml",
                self.session_id,
                running_ids.len()
            );
        }

        let run_id = running_ids[0];
        let window_name = self
            .agent_runs
            .iter()
            .find(|r| r.id == run_id)
            .map(|r| r.window_name.clone())
            .unwrap_or_default();
        let window_live = live_windows.contains(&window_name);

        if !window_live {
            // Finalize as Failed
            let run_idx = self.agent_runs.iter().position(|r| r.id == run_id).unwrap();
            self.agent_runs[run_idx].status = RunStatus::Failed;
            self.agent_runs[run_idx].ended_at = Some(chrono::Utc::now());
            self.agent_runs[run_idx].error = Some("window missing on resume".to_string());

            let duration = self.agent_runs[run_idx]
                .ended_at
                .unwrap()
                .signed_duration_since(self.agent_runs[run_idx].started_at);
            let msg = Message {
                ts: chrono::Utc::now(),
                run_id,
                kind: MessageKind::End,
                sender: MessageSender::System,
                text: format!(
                    "failed in {}s: window missing on resume",
                    duration.num_seconds()
                ),
            };
            let _ = self.append_message(&msg); // Best-effort
            self.save()?;
            return Ok(None);
        }

        Ok(Some(run_id))
    }
}

/// Return the directory path for a given session ID.
pub fn session_dir(session_id: &str) -> PathBuf {
    Path::new(".codexize").join("sessions").join(session_id)
}

#[cfg(test)]
pub(crate) fn test_fs_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};

    static TEST_FS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_FS_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = test_fs_lock().lock().unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let cwd = std::env::current_dir().unwrap();

        std::env::set_current_dir(temp.path()).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::env::set_current_dir(cwd).unwrap();
        result.unwrap()
    }

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
            sender: MessageSender::Agent {
                model: "gpt-5".to_string(),
                vendor: "openai".to_string(),
            },
            text: "Exploring codebase".to_string(),
        };

        assert_eq!(msg.run_id, 1);
        assert_eq!(msg.kind, MessageKind::Brief);
        assert_eq!(msg.text, "Exploring codebase");
    }

    #[test]
    fn test_message_kind_started_deserializes() {
        let kind = serde_json::from_str::<MessageKind>("\"Started\"");
        assert!(kind.is_ok(), "Started message kind must deserialize");
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
        with_temp_root(|| {
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
        });
    }

    #[test]
    fn test_session_state_v1_rejection() {
        with_temp_root(|| {
            // Manually write a v1 session file (no schema_version field)
            let dir = session_dir("test-v1-session");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("session.toml");
            std::fs::write(
                &path,
                r#"
session_id = "test-v1-session"
current_phase = "IdeaInput"
"#,
            )
            .unwrap();

            let result = SessionState::load("test-v1-session");
            assert!(result.is_err());
            let err_msg = format!("{:?}", result.unwrap_err());
            assert!(err_msg.contains("schema v1") || err_msg.contains("archive"));
        });
    }

    #[test]
    fn test_append_message() {
        with_temp_root(|| {
            let state = SessionState::new("test-msg-session".to_string());
            state.save().unwrap();

            let msg = Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::Brief,
                sender: MessageSender::Agent {
                    model: "gpt-5".to_string(),
                    vendor: "openai".to_string(),
                },
                text: "Exploring code".to_string(),
            };

            state.append_message(&msg).unwrap();

            // Verify file exists and contains the message
            let path = session_dir("test-msg-session").join("messages.jsonl");
            assert!(path.exists());
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.contains("Exploring code"));
        });
    }

    #[test]
    fn test_load_messages() {
        with_temp_root(|| {
            let state = SessionState::new("test-load-msg".to_string());
            state.save().unwrap();

            let msg1 = Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::Brief,
                sender: MessageSender::Agent {
                    model: "gpt-5".to_string(),
                    vendor: "openai".to_string(),
                },
                text: "First".to_string(),
            };
            let msg2 = Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: MessageSender::System,
                text: "done in 1m".to_string(),
            };

            state.append_message(&msg1).unwrap();
            state.append_message(&msg2).unwrap();

            let loaded = SessionState::load_messages("test-load-msg").unwrap();
            assert_eq!(loaded.len(), 2);
            assert_eq!(loaded[0].text, "First");
            assert_eq!(loaded[1].text, "done in 1m");
        });
    }

    #[test]
    fn test_load_messages_roundtrip_sender_field() {
        with_temp_root(|| {
            let state = SessionState::new("test-sender-msg".to_string());
            state.save().unwrap();
            let dir = session_dir("test-sender-msg");
            let path = dir.join("messages.jsonl");
            std::fs::write(
                &path,
                r#"{"ts":"2026-04-24T00:00:00Z","run_id":1,"kind":"Brief","sender":{"Agent":{"model":"gpt-5","vendor":"openai"}},"text":"hello"}
"#,
            )
            .unwrap();

            let loaded = SessionState::load_messages("test-sender-msg").unwrap();
            assert_eq!(loaded.len(), 1);

            let serialized = serde_json::to_value(&loaded[0]).unwrap();
            assert_eq!(
                serialized
                    .pointer("/sender/Agent/model")
                    .and_then(serde_json::Value::as_str),
                Some("gpt-5")
            );
            assert_eq!(
                serialized
                    .pointer("/sender/Agent/vendor")
                    .and_then(serde_json::Value::as_str),
                Some("openai")
            );
        });
    }

    #[test]
    fn test_load_messages_roundtrip_started_message() {
        with_temp_root(|| {
            let state = SessionState::new("test-started-msg".to_string());
            state.save().unwrap();
            let dir = session_dir("test-started-msg");
            let path = dir.join("messages.jsonl");
            std::fs::write(
                &path,
                r#"{"ts":"2026-04-24T00:00:00Z","run_id":1,"kind":"Started","sender":"System","text":"agent started · gpt-5 (openai)"}
"#,
            )
            .unwrap();

            let loaded = SessionState::load_messages("test-started-msg").unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].text, "agent started · gpt-5 (openai)");
            let serialized = serde_json::to_value(&loaded[0]).unwrap();
            assert_eq!(
                serialized.get("kind").and_then(serde_json::Value::as_str),
                Some("Started")
            );
        });
    }

    #[test]
    fn test_load_messages_with_corrupt_line() {
        with_temp_root(|| {
            let state = SessionState::new("test-corrupt-msg".to_string());
            state.save().unwrap();

            // Manually write messages.jsonl with one corrupt line
            let dir = session_dir("test-corrupt-msg");
            let path = dir.join("messages.jsonl");
            std::fs::write(&path, r#"{"ts":"2026-04-24T00:00:00Z","run_id":1,"kind":"Brief","sender":{"Agent":{"model":"gpt-5","vendor":"openai"}},"text":"Good"}
{corrupt json line here}
{"ts":"2026-04-24T00:01:00Z","run_id":1,"kind":"End","sender":"System","text":"done"}
"#).unwrap();

            let loaded = SessionState::load_messages("test-corrupt-msg").unwrap();
            assert_eq!(loaded.len(), 2); // Corrupt line skipped
            assert_eq!(loaded[0].text, "Good");
            assert_eq!(loaded[1].text, "done");
        });
    }

    #[test]
    fn test_next_agent_run_id() {
        let mut state = SessionState::new("test-id".to_string());
        assert_eq!(state.next_agent_run_id(), 1);

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

        assert_eq!(state.next_agent_run_id(), 2);
    }

    #[test]
    fn test_resume_one_running_live_window() {
        let mut state = SessionState::new("test-resume".to_string());
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

        let live_windows = vec!["[Brainstorm]".to_string()];
        let result = state.resume_running_runs(&live_windows);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(1));
        assert_eq!(state.agent_runs[0].status, RunStatus::Running);
    }

    #[test]
    fn test_resume_one_running_missing_window() {
        let mut state = SessionState::new("test-resume-missing".to_string());
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

        let live_windows = vec![]; // No live windows
        let result = state.resume_running_runs(&live_windows);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
        assert_eq!(state.agent_runs[0].status, RunStatus::Failed);
        assert_eq!(
            state.agent_runs[0].error,
            Some("window missing on resume".to_string())
        );
    }

    #[test]
    fn test_resume_multiple_running_runs() {
        let mut state = SessionState::new("test-resume-multi".to_string());
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
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "spec".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "openai".to_string(),
            window_name: "[Spec]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        });

        let live_windows = vec!["[Brainstorm]".to_string(), "[Spec]".to_string()];
        let result = state.resume_running_runs(&live_windows);

        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("concurrent runs"));
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
    fn test_agent_runs_roundtrip() {
        let mut state = SessionState::new("test".to_string());
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
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
        });
        let toml = toml::to_string(&state).unwrap();
        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert_eq!(loaded.agent_runs.len(), 1);
        assert_eq!(loaded.agent_runs[0].id, 1);
        assert_eq!(loaded.agent_runs[0].stage, "brainstorm");
        assert_eq!(loaded.agent_runs[0].status, RunStatus::Done);
    }

    #[test]
    fn test_session_state_archived_defaults_false_on_deserialize() {
        let state = SessionState::new("test-session".to_string());
        let toml = toml::to_string(&state).unwrap();
        let loaded: SessionState = toml::from_str(&toml).unwrap();
        assert!(!loaded.archived);
    }

    #[test]
    fn test_agent_runs_defaults_empty() {
        let state = SessionState::new("test".to_string());
        assert!(state.agent_runs.is_empty());
    }

    #[test]
    fn test_schema_version_defaults_to_2() {
        let state = SessionState::new("test".to_string());
        assert_eq!(state.schema_version, 2);
    }
}
