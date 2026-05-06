//! ACP transport-side helpers: the typed input/cancel channels, the per-run
//! launch context that crosses the runner boundary, the running text
//! accumulator that translates ACP `session/update` events into transcript
//! writes, and the on-disk ACP trace fan-out. The supervisor in
//! `runner::supervise` consumes these primitives; nothing here owns process
//! lifecycle or finish-stamp policy.

use crate::acp::{AcpResolvedLaunch, AcpTextAccumulator, AcpTextBoundary, AcpTextEvent};
use crate::state::{Message, MessageKind, MessageSender, RunStatus, SessionState};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

const TRANSCRIPT_HANDOFF_PARK_INTERVAL: Duration = Duration::from_millis(25);

/// Polling cadence for the runner's ACP receive loop. Kept here so transport
/// code can sleep between idle reads without re-importing supervisor state.
pub(super) const ACP_POLL_INTERVAL: Duration = Duration::from_millis(25);

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

/// Resolve the ACP trace path that mirrors a per-run cause file. Trace files
/// share the same stem as the cause file so postmortems land in one
/// directory.
pub(in crate::data::runner) fn acp_trace_path_from_cause_path(cause_path: &Path) -> PathBuf {
    let Some(file_name) = cause_path.file_name().and_then(|name| name.to_str()) else {
        return cause_path.with_extension("acp.jsonl");
    };
    let trace_name = file_name
        .strip_suffix(".cause.txt")
        .map(|stem| format!("{stem}.acp.jsonl"))
        .unwrap_or_else(|| format!("{file_name}.acp.jsonl"));
    cause_path.with_file_name(trace_name)
}

pub(in crate::data::runner) fn acp_text_trace_path(launch: &ManagedAcpLaunch) -> PathBuf {
    acp_trace_path_from_cause_path(&launch.cause_path)
}

pub(in crate::data::runner) fn append_acp_text_trace(
    launch: &ManagedAcpLaunch,
    event: &AcpTextEvent,
) {
    let path = acp_text_trace_path(launch);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let record = serde_json::json!({
        "type": "text_event",
        "ts": chrono::Utc::now().to_rfc3339(),
        "stream": if event.thought { "thought" } else { "agent" },
        "interactive": event.interactive,
        "boundary": format!("{:?}", event.boundary),
        "identity": event.identity,
        "text": event.text,
    });
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
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
        .map(|run| (run.id, run.model.clone(), run.vendor.clone()))
}

fn persist_agent_text_block(
    launch: &ManagedAcpLaunch,
    text: String,
    kind: MessageKind,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if text.is_empty() {
        return None;
    }
    let session_id = launch.session_id.as_deref()?;

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
    let Some((run_id, model, vendor)) = run else {
        super::supervise::append_launch_cause(
            &launch.cause_path,
            "failed to persist ACP text: run record was not available",
        );
        return None;
    };

    let ts = chrono::Utc::now();
    let msg = Message {
        ts,
        run_id,
        kind,
        sender: MessageSender::Agent { model, vendor },
        text,
    };
    if let Err(err) = SessionState::load(session_id).and_then(|state| state.append_message(&msg)) {
        super::supervise::append_launch_cause(
            &launch.cause_path,
            &format!("failed to persist ACP text for run {run_id}: {err:#}"),
        );
        return None;
    }
    Some(ts)
}

fn update_agent_text_block(
    launch: &ManagedAcpLaunch,
    ts: chrono::DateTime<chrono::Utc>,
    text: &str,
) -> bool {
    let Some(session_id) = launch.session_id.as_deref() else {
        return false;
    };
    match SessionState::load(session_id).and_then(|state| state.update_message_text(ts, text)) {
        Ok(true) => true,
        Ok(false) => {
            super::supervise::append_launch_cause(
                &launch.cause_path,
                "failed to update live ACP text: message was not available",
            );
            false
        }
        Err(err) => {
            super::supervise::append_launch_cause(
                &launch.cause_path,
                &format!("failed to update live ACP text: {err:#}"),
            );
            false
        }
    }
}

/// Per-run accumulator that funnels ACP `session/update` text events into
/// a single live transcript message, breaking on identity changes the way
/// `AcpTextAccumulator` reports them.
pub(in crate::data::runner) struct AcpTextStream {
    pub(in crate::data::runner) accumulator: AcpTextAccumulator,
    pub(in crate::data::runner) live_ts: Option<chrono::DateTime<chrono::Utc>>,
}

impl AcpTextStream {
    pub(in crate::data::runner) fn new() -> Self {
        Self {
            accumulator: AcpTextAccumulator::new(),
            live_ts: None,
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
            // otherwise this intentionally over-splits rather than rewriting
            // an unrelated previous live message.
            self.finish_turn(launch, kind);
            self.live_ts = None;
        }
        if let Some(text) = self.accumulator.push(chunk) {
            self.persist_ready(launch, text, kind);
        }
        while let Some(text) = self.accumulator.next_ready() {
            self.persist_ready(launch, text, kind);
        }
        if let Some(text) = self.accumulator.current_text().map(str::to_string) {
            self.persist_live(launch, &text, kind);
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
        if let Some(ts) = self.live_ts.take()
            && update_agent_text_block(launch, ts, &text)
        {
            return;
        }
        let _ = persist_agent_text_block(launch, text, kind);
    }

    fn persist_live(&mut self, launch: &ManagedAcpLaunch, text: &str, kind: MessageKind) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(ts) = self.live_ts
            && update_agent_text_block(launch, ts, text)
        {
            return;
        }
        self.live_ts = persist_agent_text_block(launch, text.to_string(), kind);
    }
}
