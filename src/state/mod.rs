mod builder;
pub mod phase;
pub mod resume;
pub mod transitions;

pub use builder::BuilderState;
pub use phase::Phase;
pub use transitions::execute_transition;

use crate::{adapters::EffortLevel, selection::SelectionPhase};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

/// An event logged to the run's events.toml audit trail.
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
    FailedUnverified,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Modes {
    #[serde(default)]
    pub yolo: bool,
    #[serde(default)]
    pub cheap: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchModes {
    #[serde(default)]
    pub yolo: bool,
    #[serde(default)]
    pub cheap: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interactive: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Modes {
    pub fn launch_snapshot(self) -> LaunchModes {
        LaunchModes {
            yolo: self.yolo,
            cheap: self.cheap,
            interactive: false,
        }
    }
}

impl LaunchModes {
    pub fn effort_for(self, requested: EffortLevel, phase: SelectionPhase) -> EffortLevel {
        if self.cheap {
            EffortLevel::Low
        } else if self.yolo && matches!(phase, SelectionPhase::Idea | SelectionPhase::Planning) {
            EffortLevel::Tough
        } else {
            requested
        }
    }
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
    #[serde(default)]
    pub effort: EffortLevel,
    #[serde(default)]
    pub modes: LaunchModes,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub mount_device_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageKind {
    Started,
    Brief,
    AgentText,
    Summary,
    /// A summary that flags non-success verdicts (e.g., reviewer asked
    /// for revisions). Rendered as a warning rather than green success.
    SummaryWarn,
    End,
}

impl MessageKind {
    pub fn visible_with_agent_text_filter(self, show_agent_text: bool) -> bool {
        !matches!(self, Self::AgentText) || show_agent_text
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EventsFile {
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MessagesFile {
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    Stage,
    Task,
    Round,
    Mode,
    AgentRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Pending,
    Running,
    WaitingUser,
    Done,
    Skipped,
    Failed,
    FailedUnverified,
}

impl NodeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingUser => "waiting-user",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::FailedUnverified => "failed-unverified",
        }
    }

    pub fn style(self) -> ratatui::style::Style {
        use ratatui::style::{Color, Style};
        match self {
            Self::Pending => Style::default().fg(Color::DarkGray),
            Self::Running => Style::default().fg(Color::Cyan),
            Self::WaitingUser => Style::default().fg(Color::Yellow),
            Self::Done => Style::default().fg(Color::Green),
            Self::Skipped => Style::default().fg(Color::Yellow),
            Self::Failed => Style::default().fg(Color::Red),
            Self::FailedUnverified => Style::default().fg(Color::LightYellow),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PipelineItemStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Approved,
    Revise,
    HumanBlocked,
    AgentPivot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineItem {
    pub id: u32,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    pub status: PipelineItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive: Option<bool>,
}

/// The persisted state of a single codexize session.
/// A non-coder run that produced an unauthorized HEAD advance under
/// `GuardMode::AskOperator`. Persisted on `SessionState` until the operator
/// chooses reset or keep so process restarts cannot lose the decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingGuardDecision {
    pub stage: String,
    #[serde(default)]
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
    pub run_id: u64,
    pub captured_head: String,
    pub current_head: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: String,
    pub schema_version: u32,
    #[serde(default)]
    pub modes: Modes,
    #[serde(default)]
    pub agent_runs: Vec<RunRecord>,
    pub current_phase: Phase,
    #[serde(default)]
    pub idea_text: Option<String>,
    /// Operator-facing session title — set by the brainstormer once the spec
    /// is drafted. Falls back to truncated `idea_text` for display.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub show_noninteractive_texts: bool,
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
    #[serde(default)]
    pub pending_guard_decision: Option<PendingGuardDecision>,
}

impl SessionState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            schema_version: 2,
            modes: Modes::default(),
            agent_runs: Vec::new(),
            current_phase: Phase::IdeaInput,
            idea_text: None,
            title: None,
            selected_model: None,
            show_noninteractive_texts: false,
            agent_error: None,
            builder: BuilderState::default(),
            archived: false,
            skip_to_impl_rationale: None,
            skip_to_impl_kind: None,
            pending_guard_decision: None,
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

    /// Append an event to the session's events.toml audit trail.
    pub fn log_event(&self, message: impl Into<String>) -> Result<()> {
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)?;
        reject_old_artifact(&dir.join("events.jsonl"))?;
        let path = dir.join("events.toml");

        let event = Event {
            timestamp: chrono::Utc::now().to_rfc3339(),
            phase: self.current_phase,
            message: message.into(),
        };

        let mut file = read_events_file(&path)?;
        file.events.push(event);
        let text = toml::to_string_pretty(&file).context("failed to serialize events")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write events to {}", path.display()))?;
        Ok(())
    }

    /// Transition to a new phase with validation and persistence.
    pub fn transition_to(&mut self, next_phase: Phase) -> Result<()> {
        execute_transition(self, next_phase)
    }

    /// Append a message to the session's messages.toml file.
    pub fn append_message(&self, message: &Message) -> Result<()> {
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)?;
        reject_old_artifact(&dir.join("messages.jsonl"))?;
        let path = dir.join("messages.toml");

        let mut file = read_messages_file(&path)?;
        file.messages.push(message.clone());
        let text = toml::to_string_pretty(&file).context("failed to serialize messages")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write messages to {}", path.display()))?;
        Ok(())
    }

    /// Load all messages for a session from messages.toml.
    pub fn load_messages(session_id: &str) -> Result<Vec<Message>> {
        let dir = session_dir(session_id);
        reject_old_artifact(&dir.join("messages.jsonl"))?;
        let path = dir.join("messages.toml");
        if !path.exists() {
            return Ok(Vec::new());
        }

        Ok(read_messages_file(&path)?.messages)
    }

    /// Remove persisted messages whose run id is in `run_ids`.
    pub fn remove_messages_for_runs(
        &self,
        run_ids: &std::collections::BTreeSet<u64>,
    ) -> Result<()> {
        if run_ids.is_empty() {
            return Ok(());
        }
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)?;
        reject_old_artifact(&dir.join("messages.jsonl"))?;
        let path = dir.join("messages.toml");
        if !path.exists() {
            return Ok(());
        }

        let mut file = read_messages_file(&path)?;
        file.messages
            .retain(|message| !run_ids.contains(&message.run_id));
        let text = toml::to_string_pretty(&file).context("failed to serialize messages")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write messages to {}", path.display()))?;
        Ok(())
    }

    /// Create a new RunRecord, push it to agent_runs, and return its id.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run_record(
        &mut self,
        stage: String,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
        model: String,
        vendor: String,
        window_name: String,
        effort: EffortLevel,
        modes: LaunchModes,
    ) -> u64 {
        let id = self.next_agent_run_id();
        let hostname = Self::capture_hostname();
        let mount_device_id = Self::capture_mount_device_id();
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
            effort,
            modes,
            hostname,
            mount_device_id,
        };
        self.agent_runs.push(run);
        id
    }

    /// Capture current hostname for same-host resume validation.
    fn capture_hostname() -> Option<String> {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|out| {
                if out.status.success() {
                    String::from_utf8(out.stdout).ok()
                } else {
                    None
                }
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Capture device ID of the mount containing the worktree's `.git` path.
    fn capture_mount_device_id() -> Option<u64> {
        let git_path = std::env::current_dir().ok()?.join(".git");
        Self::capture_mount_device_id_for_path(&git_path)
    }

    fn capture_mount_device_id_for_path(path: &std::path::Path) -> Option<u64> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path).ok().map(|m| m.dev())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Return the next available agent_run_id (monotonic within session).
    pub fn next_agent_run_id(&self) -> u64 {
        self.agent_runs.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }

    pub fn launch_modes(&self) -> LaunchModes {
        self.modes.launch_snapshot()
    }

    /// Resume running runs on session load.
    ///
    /// Returns the current run ID if exactly one `Running` run exists after
    /// applying same-host identity validation. The app is responsible for
    /// routing missing-window runs through the drain barrier and finish-stamp
    /// finalization path.
    pub fn resume_running_runs(&mut self, live_windows: &[String]) -> Result<Option<u64>> {
        // Check for hostname/device identity mismatch first
        let current_hostname = Self::capture_hostname();
        let current_device_id = Self::capture_mount_device_id();

        // Collect messages to append after the loop
        let mut messages_to_append = Vec::new();
        let mut events_to_log = Vec::new();

        // Finalize any Running records with hostname or device mismatch
        for run in &mut self.agent_runs {
            if run.status != RunStatus::Running {
                continue;
            }
            let mut mismatch_reason = None;
            if let (Some(run_hostname), Some(current)) =
                (run.hostname.as_deref(), current_hostname.as_deref())
                && run_hostname != current
            {
                mismatch_reason = Some(format!(
                    "hostname mismatch: run={run_hostname}, current={current}"
                ));
            }
            if mismatch_reason.is_none()
                && let (Some(run_dev), Some(current_dev)) = (run.mount_device_id, current_device_id)
                && run_dev != current_dev
            {
                mismatch_reason = Some(format!(
                    "mount device mismatch: run={run_dev}, current={current_dev}"
                ));
            }
            if let Some(reason) = mismatch_reason {
                let ended_at = chrono::Utc::now();
                run.status = RunStatus::FailedUnverified;
                run.ended_at = Some(ended_at);
                run.error = Some(reason.clone());
                let duration = ended_at.signed_duration_since(run.started_at);
                let msg = Message {
                    ts: chrono::Utc::now(),
                    run_id: run.id,
                    kind: MessageKind::End,
                    sender: MessageSender::System,
                    text: format!(
                        "failed-unverified in {}s: {}",
                        duration.num_seconds(),
                        reason
                    ),
                };
                messages_to_append.push(msg);
                events_to_log.push(format!(
                    "run {} failed-unverified on resume: {}",
                    run.id, reason
                ));
            }
        }

        // Append collected messages and events
        for msg in messages_to_append {
            let _ = self.append_message(&msg);
        }
        for event in events_to_log {
            let _ = self.log_event(event);
        }

        let running_ids: Vec<u64> = self
            .agent_runs
            .iter()
            .filter(|r| r.status == RunStatus::Running)
            .map(|r| r.id)
            .collect();

        if running_ids.is_empty() {
            self.save()?;
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
        let _window_live = live_windows.contains(&window_name);

        self.save()?;
        Ok(Some(run_id))
    }
}

/// Root directory for all session state. Honors the `CODEXIZE_ROOT` env var
/// (used by tests to point at a tempdir); defaults to `.codexize` in the
/// current working directory for normal use.
pub fn codexize_root() -> PathBuf {
    std::env::var_os("CODEXIZE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".codexize"))
}

/// Return the directory path for a given session ID.
pub fn session_dir(session_id: &str) -> PathBuf {
    codexize_root().join("sessions").join(session_id)
}

fn reject_old_artifact(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        anyhow::bail!(
            "unsupported old JSON/JSONL session artifact {}; start a fresh TOML session",
            path.display()
        );
    }
    Ok(())
}

fn read_events_file(path: &std::path::Path) -> Result<EventsFile> {
    if !path.exists() {
        return Ok(EventsFile::default());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read events from {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse events from {}", path.display()))
}

fn read_messages_file(path: &std::path::Path) -> Result<MessagesFile> {
    if !path.exists() {
        return Ok(MessagesFile::default());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read messages from {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse messages from {}", path.display()))
}

#[cfg(test)]
pub(crate) fn test_fs_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};

    static TEST_FS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_FS_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests_mod;
