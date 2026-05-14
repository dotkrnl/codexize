//! Side-effect boundary types used by the runtime to drive the data layer.
//!
//! `app_runtime` consumes [`DataEvent`]s drained from observers and dispatches
//! [`DataRequest`]s when logic decides a side effect should happen, receiving
//! a typed [`DataOutcome`] back. The data layer keeps custody of the world
//! (filesystem, runner registry, providers); logic stays pure; the runtime
//! routes between them.
//!
//! This is the shape that lets a future server-mode binary reuse `data` and
//! `logic` without pulling in the TUI: the same request/event types mediate
//! every interaction. Today the App still calls some primitives directly,
//! but those calls go through the data layer's public surface — the runtime
//! seam is just the reified version of that surface as enums.
use crate::data::observation::{LiveSummaryProbe, LiveSummarySnapshot};
use std::path::PathBuf;
use tokio::sync::mpsc;
/// Facts emitted by the data layer that the runtime drains each tick.
///
/// Events are produced by long-lived observers (the live-summary `notify`
/// watcher). They carry just enough context for `app_runtime` to identify
/// which run/path the event belongs to; the runtime decides what UI updates
/// or follow-up requests to issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataEvent {
    /// The live-summary watcher reported a write. The runtime should
    /// (re-)read the live-summary file via [`DataRequest::ReadLiveSummary`]
    /// and update the watchdog idle clock for the active run.
    LiveSummaryChanged,
}
/// Typed drain handle for the live-summary `notify` watcher. Holds the raw
/// notify channel as an implementation detail and adapts each filesystem
/// notification into a [`DataEvent::LiveSummaryChanged`] so the runtime never
/// has to know that the underlying signal is an `mpsc::Receiver<()>`.
///
/// Coalescing is the caller's responsibility: a single tick of writes may
/// produce multiple `LiveSummaryChanged` events, but they are idempotent —
/// the runtime re-reads the file once per non-empty drain.
pub struct LiveSummaryEvents {
    rx: mpsc::UnboundedReceiver<()>,
}
impl LiveSummaryEvents {
    /// Wrap a notify receiver. Constructed by [`crate::data::observation`]
    /// when a watcher is built; not part of the public seam since the rx is
    /// an internal detail.
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<()>) -> Self {
        Self { rx }
    }
    /// Drain every pending watcher signal as a typed [`DataEvent`]. Returns
    /// an empty vector when nothing is queued. Non-blocking.
    pub fn drain(&mut self) -> Vec<DataEvent> {
        let mut out = Vec::new();
        while self.rx.try_recv().is_ok() {
            out.push(DataEvent::LiveSummaryChanged);
        }
        out
    }
}
impl std::fmt::Debug for LiveSummaryEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSummaryEvents").finish_non_exhaustive()
    }
}
/// Side-effect requests dispatched by the runtime to the data layer.
///
/// Each request maps onto a primitive in [`crate::data`]. Variants cover the
/// observation and runner-registry surface relocated by this refactor.
/// Future slices may extend this enum with artifact writes, event-log
/// appends, and agent spawns — `app_runtime` is the only caller of dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataRequest {
    /// Cheap mtime-only probe of the live-summary file at `path`.
    ProbeLiveSummary { path: PathBuf },
    /// Full content read of the live-summary file at `path`. Returns `None`
    /// when missing/stale/unreadable.
    ReadLiveSummary { path: PathBuf },
    /// Final read followed by removal of the live-summary file at `path`.
    /// Used at end-of-run to capture any trailing summary before cleanup.
    DrainLiveSummary { path: PathBuf },
    /// Read a prompt-body file (used to compose watchdog warnings).
    ReadPromptBody { path: PathBuf },
    /// Best-effort interrupt of the ACP run with `text` queued as the
    /// next prompt (spec §3.4 watchdog warning).
    InterruptRun { run_id: u64, text: String },
    /// Best-effort terminate of the ACP run (spec §3.5 watchdog kill).
    TerminateRun { run_id: u64 },
}
/// Typed outcome returned by [`dispatch`] for each request variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataOutcome {
    LiveSummaryProbed(LiveSummaryProbe),
    LiveSummaryRead(Option<LiveSummarySnapshot>),
    LiveSummaryDrained(Option<LiveSummarySnapshot>),
    PromptBodyRead(Option<String>),
    /// `true` when the interrupt was queued; `false` when the run was not
    /// active or had already finished.
    Interrupted(bool),
    /// `true` when the terminate signal was queued; `false` when the run
    /// was not active or had already finished.
    Terminated(bool),
}
/// Dispatch a [`DataRequest`] to the appropriate data primitive.
pub fn dispatch(
    request: DataRequest,
    runner_supervisor: &crate::data::runner::Supervisor,
) -> DataOutcome {
    match dispatch_observation(&request) {
        Some(outcome) => outcome,
        None => match request {
            DataRequest::InterruptRun { run_id, text } => {
                DataOutcome::Interrupted(runner_supervisor.force_interrupt_run(run_id, text))
            }
            DataRequest::TerminateRun { run_id } => {
                DataOutcome::Terminated(runner_supervisor.terminate_run(run_id))
            }
            DataRequest::ProbeLiveSummary { .. }
            | DataRequest::ReadLiveSummary { .. }
            | DataRequest::DrainLiveSummary { .. }
            | DataRequest::ReadPromptBody { .. } => {
                tracing::error!("dispatch_observation should have handled {request:?}");
                DataOutcome::PromptBodyRead(None)
            }
        },
    }
}
/// Dispatch the observation-only subset of [`DataRequest`] without consulting
/// the runner registry. Returns `None` for variants that need a `Supervisor`
/// (interrupt/terminate); callers that never issue those variants — e.g. the
/// headless runtime's live-summary status path — can use this directly and
/// skip the supervisor argument entirely.
pub fn dispatch_observation(request: &DataRequest) -> Option<DataOutcome> {
    Some(match request {
        DataRequest::ProbeLiveSummary { path } => {
            DataOutcome::LiveSummaryProbed(crate::data::observation::probe_live_summary(path))
        }
        DataRequest::ReadLiveSummary { path } => {
            DataOutcome::LiveSummaryRead(crate::data::observation::read_live_summary(path))
        }
        DataRequest::DrainLiveSummary { path } => {
            DataOutcome::LiveSummaryDrained(crate::data::observation::drain_live_summary_file(path))
        }
        DataRequest::ReadPromptBody { path } => {
            DataOutcome::PromptBodyRead(crate::data::observation::read_prompt_body(path))
        }
        DataRequest::InterruptRun { .. } | DataRequest::TerminateRun { .. } => return None,
    })
}
#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
