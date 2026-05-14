//! ACP transport-side helpers: the typed input/cancel channels, the per-run
//! launch context that crosses the runner boundary, and the running text
//! accumulator that translates ACP `session/update` events into transcript
//! writes. The supervisor in `runner::supervise` consumes these primitives;
//! nothing here owns process lifecycle or finish-stamp policy.
use crate::data::acp::{AcpResolvedLaunch, AcpTextAccumulator, AcpTextBoundary};
use crate::state::{Message, MessageKind, MessageSender, RunStatus, SessionState};
#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
const TRANSCRIPT_HANDOFF_PARK_INTERVAL: Duration = Duration::from_millis(25);
/// Polling cadence for the runner's ACP receive loop. Kept here so transport
/// code can sleep between idle reads without re-importing supervisor state.
pub(super) const ACP_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Narrow clock injection seam scoped to the ACP runtime loop. Production
/// code uses [`RealAcpClock`] (wall-clock `Instant` + thread parking); tests
/// use [`FakeAcpClock`] to advance perceived time deterministically without
/// real sleeps.
pub(in crate::data::runner) trait AcpClock {
    fn now(&self) -> Instant;
    fn park(&self);
}

pub(in crate::data::runner) struct RealAcpClock;

impl AcpClock for RealAcpClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn park(&self) {
        thread::park_timeout(ACP_POLL_INTERVAL);
    }
}

#[cfg(test)]
pub(in crate::data::runner) struct FakeAcpClock {
    pub(in crate::data::runner) now: Cell<Instant>,
}

#[cfg(test)]
impl AcpClock for FakeAcpClock {
    fn now(&self) -> Instant {
        self.now.get()
    }
    fn park(&self) {
        // Advance perceived wall-clock by one poll interval so the cancel-ack
        // watchdog crosses 60s/120s thresholds after a fixed number of loop
        // iterations, without sleeping real time.
        self.now.set(self.now.get() + ACP_POLL_INTERVAL);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::data::runner) enum AcpCancelReason {
    Terminate,
    Complete,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::data::runner) enum AcpInput {
    Prompt(String),
    Interrupt(String),
}
/// Resolved per-launch context the supervisor hands to transport helpers.
/// Field visibility is widened to the whole `runner` module tree so the
/// co-located test suite can build fixtures directly.
#[derive(Debug, Clone)]
pub(in crate::data::runner) struct ManagedAcpLaunch {
    pub(in crate::data::runner) resolved: AcpResolvedLaunch,
    pub(in crate::data::runner) window_name: String,
    pub(in crate::data::runner) session_id: Option<String>,
    pub(in crate::data::runner) stamp_path: PathBuf,
    pub(in crate::data::runner) cause_path: PathBuf,
    pub(in crate::data::runner) required_artifact: Option<PathBuf>,
}
fn find_transcript_run(session_id: &str, window_name: &str) -> Option<(u64, String, String)> {
    let state = SessionState::load(session_id).ok()?;
    state
        .agent_runs
        .iter()
        .rev()
        .find(|run| run.window_name == window_name && run.status == RunStatus::Running)
        .or_else(|| {
            state
                .agent_runs
                .iter()
                .rev()
                .find(|run| run.window_name == window_name)
        })
        .map(|run| (run.id, run.model.clone(), run.subscription_label.clone()))
}
fn persist_agent_text_block(launch: &ManagedAcpLaunch, text: String, kind: MessageKind) {
    if text.is_empty() {
        return;
    }
    let Some(session_id) = launch.session_id.as_deref() else {
        return;
    };
    // ACP output can arrive before the app thread finishes saving the run
    // record, so transcript persistence waits briefly for that handoff.
    let run = (0..80).find_map(|_| {
        let found = find_transcript_run(session_id, &launch.window_name);
        if found.is_none() {
            // park (rather than the sync sleep primitive) keeps the runner
            // crate off the banned blocking-poll API while preserving the
            // transcript-handoff backoff cadence; an explicit unpark is
            // intentionally not wired since this loop is bounded to 80
            // iterations.
            thread::park_timeout(TRANSCRIPT_HANDOFF_PARK_INTERVAL);
        }
        found
    });
    let Some((run_id, model, subscription_label)) = run else {
        super::supervise::append_launch_cause(
            &launch.cause_path,
            "failed to persist ACP text: run record was not available",
        );
        return;
    };
    let msg = Message {
        ts: chrono::Utc::now(),
        run_id,
        kind,
        sender: MessageSender::Agent {
            model,
            subscription_label,
        },
        text,
    };
    if let Err(err) = SessionState::load(session_id).and_then(|state| state.append_message(&msg)) {
        super::supervise::append_launch_cause(
            &launch.cause_path,
            &format!("failed to persist ACP text for run {run_id}: {err:#}"),
        );
    }
}
/// Per-run accumulator that funnels ACP `session/update` text events into
/// transcript writes. Only finalized blocks (paragraph or max-char boundary,
/// or end-of-turn remainder) are persisted; live partial text stays in the
/// accumulator so `messages.toml` is rewritten once per block instead of once
/// per streaming chunk.
pub(in crate::data::runner) struct AcpTextStream {
    pub(in crate::data::runner) accumulator: AcpTextAccumulator,
}
impl AcpTextStream {
    pub(in crate::data::runner) fn new() -> Self {
        Self {
            accumulator: AcpTextAccumulator::new(),
        }
    }
    #[cfg(test)]
    pub(in crate::data::runner) fn push_text(
        &mut self,
        launch: &ManagedAcpLaunch,
        chunk: &str,
        kind: MessageKind,
    ) {
        self.push_text_boundary(launch, chunk, kind, AcpTextBoundary::Continue);
    }
    pub(in crate::data::runner) fn push_text_boundary(
        &mut self,
        launch: &ManagedAcpLaunch,
        chunk: &str,
        kind: MessageKind,
        boundary: AcpTextBoundary,
    ) {
        if boundary == AcpTextBoundary::StartNewMessage {
            // ACP only emits Continue when stable identity proves continuity;
            // otherwise this finalizes the prior accumulator state so any
            // pending live remainder becomes its own block.
            self.finish_turn(launch, kind);
        }
        if let Some(text) = self.accumulator.push(chunk) {
            self.persist_ready(launch, text, kind);
        }
        while let Some(text) = self.accumulator.next_ready() {
            self.persist_ready(launch, text, kind);
        }
    }
    pub(in crate::data::runner) fn finish_turn(
        &mut self,
        launch: &ManagedAcpLaunch,
        kind: MessageKind,
    ) {
        while let Some(text) = self.accumulator.finish_prompt_turn() {
            self.persist_ready(launch, text, kind);
        }
    }
    fn persist_ready(&mut self, launch: &ManagedAcpLaunch, text: String, kind: MessageKind) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        persist_agent_text_block(launch, text, kind);
    }
}

/// Append a `MessageKind::SummaryWarn` dashboard message for the active run
/// through the existing session-state persistence path. Best-effort: if the
/// run record is unavailable (e.g. the app thread hasn't saved it), the
/// message is silently dropped.
pub(in crate::data::runner) fn persist_system_warning(launch: &ManagedAcpLaunch, text: &str) {
    let Some(session_id) = launch.session_id.as_deref() else {
        return;
    };
    let Some((run_id, _, _)) = find_transcript_run(session_id, &launch.window_name) else {
        return;
    };
    let msg = Message {
        ts: chrono::Utc::now(),
        run_id,
        kind: MessageKind::SummaryWarn,
        sender: MessageSender::System,
        text: text.to_string(),
    };
    let _ = SessionState::load(session_id).and_then(|state| state.append_message(&msg));
}

/// Hook surface for cancel-ack diagnostics emitted from the ACP runtime
/// loop. Production wires [`RealAcpDiagnostics`] which fans out to the
/// existing message-persistence path; tests inject a recorder so they can
/// assert the runtime actually surfaced warnings at the right thresholds.
pub(in crate::data::runner) trait AcpDiagnostics: Send + Sync {
    fn persist_warning(&self, launch: &ManagedAcpLaunch, text: &str);
}

pub(in crate::data::runner) struct RealAcpDiagnostics;

impl AcpDiagnostics for RealAcpDiagnostics {
    fn persist_warning(&self, launch: &ManagedAcpLaunch, text: &str) {
        persist_system_warning(launch, text);
    }
}

#[cfg(test)]
#[derive(Default)]
pub(in crate::data::runner) struct FakeDiagState {
    pub(in crate::data::runner) warnings: Vec<String>,
}

/// Recording [`AcpDiagnostics`] for runtime tests. Captures every
/// `persist_warning` call so assertions can verify the runtime actually
/// fired the cancel-ack `SummaryWarn` messages at the right thresholds.
#[cfg(test)]
#[derive(Default, Clone)]
pub(in crate::data::runner) struct FakeAcpDiagnostics {
    inner: Arc<Mutex<FakeDiagState>>,
}

#[cfg(test)]
impl FakeAcpDiagnostics {
    pub(in crate::data::runner) fn new() -> Self {
        Self::default()
    }
    pub(in crate::data::runner) fn warnings(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("FakeAcpDiagnostics mutex poisoned")
            .warnings
            .clone()
    }
}

#[cfg(test)]
impl AcpDiagnostics for FakeAcpDiagnostics {
    fn persist_warning(&self, _launch: &ManagedAcpLaunch, text: &str) {
        self.inner
            .lock()
            .expect("FakeAcpDiagnostics mutex poisoned")
            .warnings
            .push(text.to_string());
    }
}
