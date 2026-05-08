//! Notification publisher driven by the unified config.
//!
//! The old `NtfyConfig` struct and `~/.codexize/ntfy.toml` loader have been
//! removed — notifications now read from `NtfyView` / `NtfyEventsView` in
//! the unified `data::config` module. All formerly hardcoded constants
//! (`NTFY_BODY_MAX_BYTES`, retry counts, etc.) are now read at run time
//! through `NotificationParams`.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::data::config::schema::NtfyDetailMode;
use crate::data::config::view::{NtfyEventsView, NtfyView};
use crate::state::Phase;

const TOPIC_BYTES: usize = 16;
const NTFY_TITLE_MAX_CHARS: usize = 80;

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
    pub last_live_summary: Option<String>,
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

/// Extracted notification parameters for the runtime. Carries all
/// per-message knobs from the loaded config so the publisher is fully
/// config-driven.
#[derive(Debug, Clone)]
pub struct NotificationParams {
    pub enabled: bool,
    pub server: String,
    pub topic: String,
    pub detail_mode: NtfyDetailMode,
    pub body_max_bytes: u64,
    pub excerpt_max_chars: u32,
    pub retry_attempts: u32,
    pub retry_delay_ms: u64,
    pub http_timeout_secs: u32,
    pub events: NtfyEventsView,
}

impl NotificationParams {
    /// Build params from the loaded config, disabling notifications when
    /// the topic is empty (the default / "minted later" state).
    pub fn from_view(ntfy: &NtfyView) -> Self {
        Self {
            enabled: ntfy.enabled && !ntfy.topic.is_empty(),
            server: ntfy.server.clone(),
            topic: ntfy.topic.clone(),
            detail_mode: ntfy.detail_mode,
            body_max_bytes: ntfy.body_max_bytes,
            excerpt_max_chars: ntfy.excerpt_max_chars,
            retry_attempts: ntfy.retry_attempts,
            retry_delay_ms: ntfy.retry_delay_ms,
            http_timeout_secs: ntfy.http_timeout_secs,
            events: ntfy.events,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NtfyMessage {
    pub(crate) title: String,
    pub(crate) body: String,
}

pub struct NotificationRuntime {
    params: NotificationParams,
    client: Option<Client>,
    report_tx: mpsc::UnboundedSender<String>,
    report_rx: mpsc::UnboundedReceiver<String>,
    pending_sends: Vec<JoinHandle<()>>,
    occurrence: u64,
    seen: HashSet<NotificationDedupeKey>,
    events: Vec<NotificationEvent>,
}

impl NotificationRuntime {
    pub fn new(params: NotificationParams) -> Self {
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        let client = if params.enabled {
            ntfy_http_client(params.http_timeout_secs).ok()
        } else {
            None
        };
        Self {
            params,
            client,
            report_tx,
            report_rx,
            pending_sends: Vec::new(),
            occurrence: 0,
            seen: HashSet::new(),
            events: Vec::new(),
        }
    }

    pub fn new_disabled() -> Self {
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        Self {
            params: NotificationParams {
                enabled: false,
                server: String::new(),
                topic: String::new(),
                detail_mode: NtfyDetailMode::Detailed,
                body_max_bytes: 0,
                excerpt_max_chars: 0,
                retry_attempts: 0,
                retry_delay_ms: 0,
                http_timeout_secs: 0,
                events: NtfyEventsView {
                    phase_wait: true,
                    interactive_wait: true,
                    pipeline_done: true,
                },
            },
            client: None,
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
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        Self {
            params: NotificationParams {
                enabled: true,
                ..NotificationParams::from_view(&NtfyView {
                    enabled: true,
                    server: "https://ntfy.sh".to_string(),
                    topic: "test-topic".to_string(),
                    detail_mode: NtfyDetailMode::Minimal,
                    retry_attempts: 1,
                    retry_delay_ms: 0,
                    http_timeout_secs: 5,
                    body_max_bytes: 4096,
                    excerpt_max_chars: 600,
                    events: NtfyEventsView {
                        phase_wait: true,
                        interactive_wait: true,
                        pipeline_done: true,
                    },
                })
            },
            client: None,
            report_tx,
            report_rx,
            pending_sends: Vec::new(),
            occurrence: 0,
            seen: HashSet::new(),
            events: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_params_for_test(params: NotificationParams) -> Self {
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        let client = if params.enabled {
            ntfy_http_client(params.http_timeout_secs).ok()
        } else {
            None
        };
        Self {
            params,
            client,
            report_tx,
            report_rx,
            pending_sends: Vec::new(),
            occurrence: 0,
            seen: HashSet::new(),
            events: Vec::new(),
        }
    }

    pub fn events(&self) -> &[NotificationEvent] {
        &self.events
    }

    pub fn emit_phase_wait(&mut self, phase: Phase, context: NotificationContext) {
        if !self.params.enabled {
            return;
        }
        if !self.params.events.phase_wait {
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
        if !self.params.enabled {
            return;
        }
        if !self.params.events.interactive_wait {
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
        if !self.params.enabled {
            return;
        }
        if !self.params.events.pipeline_done {
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
        let server = self.params.server.clone();
        let topic = self.params.topic.clone();
        let detail_mode = self.params.detail_mode;
        let body_max_bytes = self.params.body_max_bytes;
        let excerpt_max_chars = self.params.excerpt_max_chars;
        let retry_attempts = self.params.retry_attempts;
        let retry_delay = Duration::from_millis(self.params.retry_delay_ms);
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
        let handle = tokio::spawn(async move {
            let subscribe_url = format!("{}/{}", server.trim_end_matches('/'), topic);
            let title = normalize_header_title(&prose_title(&event));
            let include_context = matches!(detail_mode, NtfyDetailMode::Detailed);
            let body = prose_body(&event, include_context, excerpt_max_chars);
            let message = NtfyMessage {
                title,
                body: truncate_body(body, body_max_bytes),
            };
            let attempts = retry_attempts.max(1) as usize;
            let mut last_error: Option<anyhow::Error> = None;
            for attempt in 1..=attempts {
                match publish_once_with_client(&client, &subscribe_url, &message).await {
                    Ok(()) => return,
                    Err(err) => {
                        last_error = Some(err);
                        if attempt < attempts && !retry_delay.is_zero() {
                            tokio::time::sleep(retry_delay).await;
                        }
                    }
                }
            }
            let last = last_error
                .map(|err| format!("{err:#}"))
                .unwrap_or_else(|| "unknown publish error".to_string());
            let _ = tx.send(format!(
                "ntfy publish failed after {attempts} attempts: {last}"
            ));
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

async fn publish_once_with_client(
    client: &Client,
    subscribe_url: &str,
    message: &NtfyMessage,
) -> Result<()> {
    let response = client
        .post(subscribe_url)
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

fn ntfy_http_client(timeout_secs: u32) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs.max(1) as u64))
        .build()
        .context("failed to build ntfy HTTP client")
}

pub(crate) fn generate_topic() -> Result<String> {
    let mut bytes = [0_u8; TOPIC_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|err| anyhow::anyhow!("failed to generate ntfy topic entropy: {err}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

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

fn prose_body(event: &NotificationEvent, include_context: bool, excerpt_max_chars: u32) -> String {
    let mut body = lead_sentence(event);
    if include_context && let Some(line) = context_line(event, excerpt_max_chars) {
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

fn context_line(event: &NotificationEvent, excerpt_max_chars: u32) -> Option<String> {
    match event.reason {
        NotificationReason::InteractiveRunWait => event
            .context
            .last_agent_response
            .as_deref()
            .and_then(|text| non_empty_excerpt(text, excerpt_max_chars))
            .map(|excerpt| format!("Last response: {excerpt}")),
        NotificationReason::PhaseWait => event
            .context
            .last_live_summary
            .as_deref()
            .and_then(|text| non_empty_excerpt(text, excerpt_max_chars))
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

fn non_empty_excerpt(text: &str, max_chars: u32) -> Option<String> {
    let collapsed = collapse_whitespace(text);
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate_chars(&collapsed, max_chars as usize))
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

fn truncate_body(body: String, max_bytes: u64) -> String {
    let max_bytes = max_bytes as usize;
    if body.len() <= max_bytes {
        return body;
    }
    let marker = "...";
    let mut end = max_bytes.saturating_sub(marker.len());
    while !body.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &body[..end], marker)
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
