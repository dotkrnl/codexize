use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::state::Phase;

pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";
pub(crate) const NTFY_BODY_MAX_BYTES: usize = 4096;
const NTFY_CONFIG_ENV: &str = "CODEXIZE_NTFY_CONFIG";
const NTFY_CONFIG_VERSION: u32 = 1;
const TOPIC_BYTES: usize = 16;
const NTFY_TITLE_MAX_CHARS: usize = 80;
const DEFAULT_PUBLISH_ATTEMPTS: usize = 3;
const DEFAULT_PUBLISH_DELAY: Duration = Duration::from_millis(250);
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NtfyDetailMode {
    Detailed,
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NtfyConfig {
    pub version: u32,
    pub server: String,
    pub topic: String,
    pub enabled: bool,
    pub detail_mode: NtfyDetailMode,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationEventKind {
    InputNeeded,
    PipelineDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationReason {
    PhaseWait,
    InteractiveRunWait,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationContext {
    pub session_id: String,
    pub session_label: String,
    pub stage: String,
    pub task_id: Option<u32>,
    pub round: Option<u32>,
    pub attempt: Option<u32>,
    pub run_id: Option<u64>,
    /// Last live-summary line (the agent's "what I'm doing" status).
    /// Populated for phase-wait and pipeline-done notifications so the
    /// reader sees what the agent had just finished when the pipeline
    /// stopped for them — it is the right answer to "what just happened?"
    /// for review pauses and the final "done" ping.
    pub last_live_summary: Option<String>,
    /// Last `AgentText` ACP response from the run that is now waiting on
    /// the user. Populated only for interactive-run waits, where the
    /// agent's most recent response *is* the question being asked, and
    /// surfacing it inline saves a context-switch back to the TUI.
    pub last_agent_response: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationEvent {
    pub kind: NotificationEventKind,
    pub reason: NotificationReason,
    pub phase: Phase,
    pub context: NotificationContext,
    pub dedupe_key: NotificationDedupeKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NtfyMessage {
    pub(crate) title: String,
    pub(crate) body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NtfyPublishPolicy {
    max_attempts: usize,
    retry_delay: Duration,
}

impl NtfyPublishPolicy {
    fn default_runtime() -> Self {
        Self {
            max_attempts: DEFAULT_PUBLISH_ATTEMPTS,
            retry_delay: DEFAULT_PUBLISH_DELAY,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(max_attempts: usize, retry_delay: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            retry_delay,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationDedupeKey {
    PhaseWait {
        session_id: String,
        stage: String,
        phase: Phase,
        occurrence: u64,
    },
    InteractiveRunWait {
        session_id: String,
        stage: String,
        run_id: u64,
        message_index: usize,
    },
    PipelineDone {
        session_id: String,
        occurrence: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveWaitMarker {
    pub run_id: u64,
    pub message_index: usize,
}

pub struct NotificationRuntime {
    enabled: bool,
    config: Option<NtfyConfig>,
    client: Option<Client>,
    policy: NtfyPublishPolicy,
    report_tx: mpsc::UnboundedSender<String>,
    report_rx: mpsc::UnboundedReceiver<String>,
    pending_sends: Vec<JoinHandle<()>>,
    occurrence: u64,
    seen: HashSet<NotificationDedupeKey>,
    events: Vec<NotificationEvent>,
}

impl Default for NotificationRuntime {
    fn default() -> Self {
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        Self {
            enabled: false,
            config: None,
            client: None,
            policy: NtfyPublishPolicy::default_runtime(),
            report_tx,
            report_rx,
            pending_sends: Vec::new(),
            occurrence: 0,
            seen: HashSet::new(),
            events: Vec::new(),
        }
    }
}

impl NotificationRuntime {
    pub fn from_config(config: Option<NtfyConfig>) -> Self {
        Self::from_config_with_policy(config, NtfyPublishPolicy::default_runtime())
    }

    fn from_config_with_policy(config: Option<NtfyConfig>, policy: NtfyPublishPolicy) -> Self {
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        let client = config.as_ref().and_then(|_| ntfy_http_client().ok());
        Self {
            enabled: config.is_some(),
            config,
            client,
            policy,
            report_tx,
            report_rx,
            pending_sends: Vec::new(),
            occurrence: 0,
            seen: HashSet::new(),
            events: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn enabled_for_test() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn from_config_for_test(
        config: Option<NtfyConfig>,
        policy: NtfyPublishPolicy,
    ) -> Self {
        Self::from_config_with_policy(config, policy)
    }

    pub fn events(&self) -> &[NotificationEvent] {
        &self.events
    }

    pub fn emit_phase_wait(&mut self, phase: Phase, context: NotificationContext) {
        if !self.enabled {
            return;
        }
        self.occurrence = self.occurrence.saturating_add(1);
        let dedupe_key = NotificationDedupeKey::PhaseWait {
            session_id: context.session_id.clone(),
            stage: context.stage.clone(),
            phase,
            occurrence: self.occurrence,
        };
        self.push(NotificationEvent {
            kind: NotificationEventKind::InputNeeded,
            reason: NotificationReason::PhaseWait,
            phase,
            context,
            dedupe_key,
        });
    }

    pub fn emit_interactive_wait(
        &mut self,
        phase: Phase,
        context: NotificationContext,
        marker: InteractiveWaitMarker,
    ) {
        if !self.enabled {
            return;
        }
        let dedupe_key = NotificationDedupeKey::InteractiveRunWait {
            session_id: context.session_id.clone(),
            stage: context.stage.clone(),
            run_id: marker.run_id,
            message_index: marker.message_index,
        };
        self.push(NotificationEvent {
            kind: NotificationEventKind::InputNeeded,
            reason: NotificationReason::InteractiveRunWait,
            phase,
            context,
            dedupe_key,
        });
    }

    pub fn emit_pipeline_done(&mut self, phase: Phase, context: NotificationContext) {
        if !self.enabled {
            return;
        }
        self.occurrence = self.occurrence.saturating_add(1);
        let dedupe_key = NotificationDedupeKey::PipelineDone {
            session_id: context.session_id.clone(),
            occurrence: self.occurrence,
        };
        self.push(NotificationEvent {
            kind: NotificationEventKind::PipelineDone,
            reason: NotificationReason::PhaseWait,
            phase,
            context,
            dedupe_key,
        });
    }

    fn push(&mut self, event: NotificationEvent) {
        if self.seen.insert(event.dedupe_key.clone()) {
            self.events.push(event.clone());
            self.enqueue_publish(event);
        }
    }

    fn enqueue_publish(&mut self, event: NotificationEvent) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(client) = self.client.clone() else {
            let _ = self
                .report_tx
                .send("failed to build ntfy HTTP client".to_string());
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            let _ = self
                .report_tx
                .send("no Tokio runtime available for ntfy publish".to_string());
            return;
        }
        let tx = self.report_tx.clone();
        let policy = self.policy;
        let handle = tokio::spawn(async move {
            if let Err(err) = send_ntfy_with_policy(&client, &config, &event, policy).await {
                let _ = tx.send(format!("{err:#}"));
            }
        });
        self.pending_sends.push(handle);
    }

    pub(crate) fn poll_publish_failures(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        while let Ok(failure) = self.report_rx.try_recv() {
            failures.push(failure);
        }
        self.pending_sends.retain(|handle| !handle.is_finished());
        failures
    }

    pub(crate) async fn drain_pending_sends(&mut self, timeout: Duration) -> bool {
        let pending = std::mem::take(&mut self.pending_sends);
        if pending.is_empty() {
            return true;
        }
        let wait_all = async move {
            for handle in pending {
                let _ = handle.await;
            }
        };
        // Leave completed publish reports queued for the app-level poller so
        // shutdown failures use the same event-log and status-warning path.
        tokio::time::timeout(timeout, wait_all).await.is_ok()
    }

    #[cfg(test)]
    pub(crate) fn pending_sends_for_test(&mut self) -> usize {
        self.pending_sends.retain(|handle| !handle.is_finished());
        self.pending_sends.len()
    }

    #[cfg(test)]
    pub(crate) fn push_publish_failure_for_test(&mut self, failure: &str) {
        let _ = self.report_tx.send(failure.to_string());
    }
}

pub fn phase_needs_input(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::BlockedNeedsUser
            | Phase::SpecReviewPaused
            | Phase::PlanReviewPaused
            | Phase::SkipToImplPending
            | Phase::GitGuardPending
            | Phase::DreamingPending
    )
}

impl NtfyConfig {
    pub fn subscribe_url(&self) -> String {
        format!("{}/{}", self.server.trim_end_matches('/'), self.topic)
    }
}

pub(crate) fn format_ntfy_message(config: &NtfyConfig, event: &NotificationEvent) -> NtfyMessage {
    let title = normalize_header_title(&prose_title(event));
    let include_context = matches!(config.detail_mode, NtfyDetailMode::Detailed);
    let body = prose_body(event, include_context);
    NtfyMessage {
        title,
        body: truncate_body(body),
    }
}

pub async fn send_ntfy(config: &NtfyConfig, event: &NotificationEvent) -> Result<()> {
    let client = ntfy_http_client()?;
    send_ntfy_with_policy(&client, config, event, NtfyPublishPolicy::default_runtime()).await
}

pub(crate) async fn send_ntfy_with_policy(
    client: &Client,
    config: &NtfyConfig,
    event: &NotificationEvent,
    policy: NtfyPublishPolicy,
) -> Result<()> {
    let attempts = policy.max_attempts.max(1);
    let message = format_ntfy_message(config, event);
    let mut last_error = None;
    for attempt in 1..=attempts {
        match publish_once(client, config, &message).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                if attempt < attempts && !policy.retry_delay.is_zero() {
                    tokio::time::sleep(policy.retry_delay).await;
                }
            }
        }
    }
    let last = last_error
        .map(|err| format!("{err:#}"))
        .unwrap_or_else(|| "unknown publish error".to_string());
    bail!("ntfy publish failed after {attempts} attempts: {last}")
}

async fn publish_once(client: &Client, config: &NtfyConfig, message: &NtfyMessage) -> Result<()> {
    let response = client
        .post(config.subscribe_url())
        .header("Title", message.title.as_str())
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(message.body.clone())
        .send()
        .await
        .context("ntfy POST request failed")?;
    if !response.status().is_success() {
        bail!("ntfy POST returned HTTP {}", response.status());
    }
    Ok(())
}

fn ntfy_http_client() -> Result<Client> {
    Client::builder()
        .timeout(DEFAULT_HTTP_TIMEOUT)
        .build()
        .context("failed to build ntfy HTTP client")
}

/// Maximum chars to surface from the live-summary or last agent response
/// in a notification body. Keeps a long ACP message from blowing through
/// the reader's lock-screen budget while still leaving room for the lead
/// sentence and context line.
const NTFY_EXCERPT_MAX_CHARS: usize = 600;

/// Headline shown as the ntfy push title — the *only* text most readers
/// see at a glance, so it gets a per-event hook (which review is paused,
/// what kind of decision is needed) instead of a generic
/// "input needed" / "pipeline done".
fn prose_title(event: &NotificationEvent) -> String {
    let label = match (event.kind, event.reason, event.phase) {
        (NotificationEventKind::PipelineDone, _, _) => "pipeline finished",
        (NotificationEventKind::InputNeeded, NotificationReason::InteractiveRunWait, _) => {
            "agent is waiting on you"
        }
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::SpecReviewPaused,
        ) => "spec ready for review",
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::PlanReviewPaused,
        ) => "plan ready for review",
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::SkipToImplPending,
        ) => "skip planning?",
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::GitGuardPending,
        ) => "review unauthorized commits",
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::DreamingPending,
        ) => "dreaming decision",
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::BlockedNeedsUser,
        ) => "blocked, needs you",
        (NotificationEventKind::InputNeeded, NotificationReason::PhaseWait, _) => "input needed",
    };
    format!("codexize: {label}")
}

/// Build the prose body shown under the notification title. `include_context`
/// gates the trailing "Last activity" / "Last response" excerpt so the
/// `Minimal` detail mode stays a single sentence while `Detailed` adds the
/// excerpt — which one is appended depends on the event:
/// - interactive-run waits get the agent's last ACP response (the question);
/// - phase-wait + pipeline-done get the last live-summary line (what was
///   in flight when the pipeline stopped for the operator).
fn prose_body(event: &NotificationEvent, include_context: bool) -> String {
    let mut body = lead_sentence(event);
    if include_context && let Some(line) = context_line(event) {
        body.push_str("\n\n");
        body.push_str(&line);
    }
    body
}

fn lead_sentence(event: &NotificationEvent) -> String {
    let session = quoted_session(&event.context.session_label);
    match (event.kind, event.reason, event.phase) {
        (NotificationEventKind::PipelineDone, _, _) => {
            format!("Pipeline finished on {session}.")
        }
        (NotificationEventKind::InputNeeded, NotificationReason::InteractiveRunWait, _) => {
            let stage = humanize_stage(&event.context.stage);
            format!("The {stage} agent on {session} is waiting on a reply.")
        }
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::SpecReviewPaused,
        ) => format!("Spec review is paused on {session}. Take a look and decide what's next."),
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::PlanReviewPaused,
        ) => format!("Plan review is paused on {session}. Take a look and decide what's next."),
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::SkipToImplPending,
        ) => format!(
            "Codexize thinks the spec for {session} is solid enough to skip planning. Confirm to skip directly to coding."
        ),
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::GitGuardPending,
        ) => format!(
            "The interactive run on {session} made commits without permission. Decide whether to keep them or reset."
        ),
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::DreamingPending,
        ) => format!("Dreaming is queued on {session}. Approve to run it, or skip and finish."),
        (
            NotificationEventKind::InputNeeded,
            NotificationReason::PhaseWait,
            Phase::BlockedNeedsUser,
        ) => format!("Final validation is blocked on {session}. You'll need to step in."),
        (NotificationEventKind::InputNeeded, NotificationReason::PhaseWait, _) => {
            let stage = humanize_stage(&event.context.stage);
            format!("Codexize on {session} needs your input at the {stage} stage.")
        }
    }
}

fn context_line(event: &NotificationEvent) -> Option<String> {
    match event.reason {
        NotificationReason::InteractiveRunWait => event
            .context
            .last_agent_response
            .as_deref()
            .and_then(non_empty_excerpt)
            .map(|excerpt| format!("Last response: {excerpt}")),
        NotificationReason::PhaseWait => event
            .context
            .last_live_summary
            .as_deref()
            .and_then(non_empty_excerpt)
            .map(|excerpt| format!("Last activity: {excerpt}")),
    }
}

fn quoted_session(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        "this session".to_string()
    } else {
        format!("\"{trimmed}\"")
    }
}

fn humanize_stage(stage: &str) -> String {
    let cleaned = stage.replace('-', " ");
    if cleaned.trim().is_empty() {
        "agent".to_string()
    } else {
        cleaned
    }
}

fn non_empty_excerpt(text: &str) -> Option<String> {
    let collapsed = collapse_whitespace(text);
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate_chars(&collapsed, NTFY_EXCERPT_MAX_CHARS))
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let marker = "...";
    let keep = max_chars.saturating_sub(marker.chars().count()).max(1);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str(marker);
    truncated
}

fn normalize_header_title(title: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_space = false;
    for ch in title.chars() {
        let ch = if ch.is_ascii() && !ch.is_ascii_control() {
            ch
        } else {
            ' '
        };
        if ch.is_ascii_whitespace() {
            if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        } else {
            normalized.push(ch);
            last_was_space = false;
        }
        if normalized.chars().count() >= NTFY_TITLE_MAX_CHARS {
            break;
        }
    }
    normalized.trim().to_string()
}

fn truncate_body(body: String) -> String {
    if body.len() <= NTFY_BODY_MAX_BYTES {
        return body;
    }
    let marker = "...";
    let mut end = NTFY_BODY_MAX_BYTES.saturating_sub(marker.len());
    while !body.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &body[..end], marker)
}

pub fn load_ntfy_config() -> Option<NtfyConfig> {
    load_ntfy_config_at(&ntfy_config_path())
}

pub fn ensure_ntfy_config(reset: bool) -> Result<NtfyConfig> {
    ensure_ntfy_config_at(&ntfy_config_path(), reset)
}

pub(crate) fn load_ntfy_config_at(path: &Path) -> Option<NtfyConfig> {
    let text = fs::read_to_string(path).ok()?;
    let config: NtfyConfig = toml::from_str(&text).ok()?;
    validate_enabled_config(config).ok()
}

pub(crate) fn ensure_ntfy_config_at(path: &Path, reset: bool) -> Result<NtfyConfig> {
    if !reset && let Some(config) = load_ntfy_config_at(path) {
        return Ok(config);
    }

    let now = Utc::now();
    let created_at = if reset {
        load_ntfy_config_at(path)
            .map(|config| config.created_at)
            .unwrap_or(now)
    } else {
        now
    };
    let config = NtfyConfig {
        version: NTFY_CONFIG_VERSION,
        server: DEFAULT_NTFY_SERVER.to_string(),
        topic: generate_topic()?,
        enabled: true,
        detail_mode: NtfyDetailMode::Detailed,
        created_at,
        updated_at: now,
    };
    atomic_write_config(path, &config)?;
    Ok(config)
}

pub(crate) fn generate_topic() -> Result<String> {
    let mut bytes = [0_u8; TOPIC_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|err| anyhow::anyhow!("failed to generate ntfy topic entropy: {err}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ntfy_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os(NTFY_CONFIG_ENV) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".codexize").join("ntfy.toml")
}

fn validate_enabled_config(config: NtfyConfig) -> Result<NtfyConfig> {
    if config.version != NTFY_CONFIG_VERSION {
        bail!("unsupported ntfy config version");
    }
    if !config.enabled {
        bail!("ntfy config is disabled");
    }
    if config.server.trim().is_empty() {
        bail!("ntfy server is empty");
    }
    if !valid_topic(&config.topic) {
        bail!("ntfy topic is invalid");
    }
    Ok(config)
}

fn valid_topic(topic: &str) -> bool {
    topic.len() >= 22
        && topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn atomic_write_config(path: &Path, config: &NtfyConfig) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).context("failed to create ntfy config directory")?;
    let tmp_path = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ntfy.toml"),
        std::process::id()
    ));
    let text = toml::to_string_pretty(config).context("failed to serialise ntfy config")?;
    {
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut tmp = options
            .open(&tmp_path)
            .context("failed to create temp ntfy config file")?;
        tmp.write_all(text.as_bytes())
            .context("failed to write temp ntfy config file")?;
        tmp.sync_all()
            .context("failed to sync temp ntfy config file")?;
    }
    fs::rename(&tmp_path, path).context("failed to rename temp ntfy config file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .context("failed to set ntfy config permissions")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
