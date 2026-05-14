//! Filesystem-backed `impl` block for [`SessionState`].
//!
//! Splitting the IO methods out of the type definition keeps
//! [`crate::state`] free of `std::fs`, `std::process`, and direct clock
//! reads. The struct itself lives in `src/state/types.rs`; this file extends
//! it with another `impl` block.
use crate::adapters::EffortLevel;
use crate::logic::pipeline::phase::Phase;
use crate::state::{
    Event, EventsFile, LaunchModes, Message, MessageKind, MessageSender, MessagesFile, RunRecord,
    RunStatus, SectionPart, SessionState, session_dir,
};
use anyhow::{Context, Result};
use std::fs;
impl SessionState {
    pub fn load(session_id: &str) -> Result<Self> {
        let path = session_dir(session_id).join("session.toml");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read session state from {}", path.display()))?;
        let state: SessionState = toml::from_str(&text)
            .with_context(|| format!("failed to parse session state from {}", path.display()))?;
        if state.schema_version != 4 {
            anyhow::bail!(
                "session {} uses schema v{}; this binary supports schema v4.",
                session_id,
                state.schema_version
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
        super::transitions::execute_transition(self, next_phase)
    }
    /// Append a message to the session's messages.toml file.
    pub fn append_message(&self, message: &Message) -> Result<()> {
        let dir = session_dir(&self.session_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join("messages.toml");
        let mut file = read_messages_file(&path)?;
        file.messages.push(message.clone());
        let text = toml::to_string_pretty(&file).context("failed to serialize messages")?;
        atomic_write(&path, text.as_bytes())
            .with_context(|| format!("failed to write messages to {}", path.display()))?;
        Ok(())
    }
    /// Load all messages for a session from messages.toml.
    pub fn load_messages(session_id: &str) -> Result<Vec<Message>> {
        let dir = session_dir(session_id);
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
        let path = dir.join("messages.toml");
        if !path.exists() {
            return Ok(());
        }
        let mut file = read_messages_file(&path)?;
        file.messages
            .retain(|message| !run_ids.contains(&message.run_id));
        let text = toml::to_string_pretty(&file).context("failed to serialize messages")?;
        atomic_write(&path, text.as_bytes())
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
        subscription_label: String,
        window_name: String,
        effort: EffortLevel,
        effort_mapping: crate::data::config::schema::EffortMapping,
        effort_eligible: bool,
        modes: LaunchModes,
        section_path: Option<Vec<SectionPart>>,
    ) -> u64 {
        let id = self.next_agent_run_id();
        self.create_run_record_with_id(
            id,
            stage,
            task_id,
            round,
            attempt,
            model,
            subscription_label,
            window_name,
            effort,
            effort_mapping,
            effort_eligible,
            modes,
            section_path,
        )
    }
    /// Create a RunRecord with an id already reserved by the launch path.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run_record_with_id(
        &mut self,
        id: u64,
        stage: String,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
        model: String,
        subscription_label: String,
        window_name: String,
        effort: EffortLevel,
        effort_mapping: crate::data::config::schema::EffortMapping,
        effort_eligible: bool,
        modes: LaunchModes,
        section_path: Option<Vec<SectionPart>>,
    ) -> u64 {
        let hostname = capture_hostname();
        let mount_device_id = capture_mount_device_id();
        let run = RunRecord {
            id,
            stage,
            task_id,
            round,
            attempt,
            model,
            subscription_label,
            window_name,
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort,
            effort_mapping,
            effort_eligible,
            modes,
            hostname,
            mount_device_id,
            section_path,
        };
        self.agent_runs.push(run);
        id
    }
    /// Resume running runs on session load.
    ///
    /// Returns the current run ID if exactly one `Running` run exists after
    /// applying same-host identity validation. The app is responsible for
    /// routing resumed runs through the drain barrier and finish-stamp
    /// finalization path.
    pub fn resume_running_runs(&mut self) -> Result<Option<u64>> {
        // Check for hostname/device identity mismatch first
        let current_hostname = capture_hostname();
        let current_device_id = capture_mount_device_id();
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
        self.save()?;
        Ok(Some(run_id))
    }
}
#[cfg(test)]
impl SessionState {
    /// Capture current hostname for same-host resume validation. Exposed for
    /// tests; runtime callers should rely on [`SessionState::create_run_record`].
    pub(crate) fn capture_hostname() -> Option<String> {
        capture_hostname()
    }
    /// Capture device ID of the mount containing the worktree's `.git` path.
    pub(crate) fn capture_mount_device_id() -> Option<u64> {
        capture_mount_device_id()
    }
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
    capture_mount_device_id_for_path(&git_path)
}
fn capture_mount_device_id_for_path(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
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
/// Write `bytes` to `path` atomically: serialise to a sibling temp file, then
/// rename over the target. Concurrent readers therefore see either the old
/// contents or the new contents in full — never the empty/partial window
/// `fs::write` exposes between its `O_TRUNC` and the trailing write.
///
/// `messages.toml` is the load-bearing case (the runner appends from a
/// blocking task while the main tick reloads via `update_agent_progress`,
/// and an empty TOML deserialises to an empty `Vec<Message>`, which would
/// blank out the chat pane).
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = match path.file_name() {
        Some(name) => {
            let mut tmp_name = std::ffi::OsString::from(name);
            tmp_name.push(".tmp");
            path.with_file_name(tmp_name)
        }
        None => return Err(std::io::Error::other("atomic_write: path has no filename")),
    };
    fs::write(&tmp, bytes)?;
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}
