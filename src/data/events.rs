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

use std::path::PathBuf;
use std::sync::mpsc;

use crate::data::observation::{LiveSummaryProbe, LiveSummarySnapshot};
use crate::data::runner::ToolCallTransition;

/// Facts emitted by the data layer that the runtime drains each tick.
///
/// Events are produced by long-lived observers (the live-summary `notify`
/// watcher; the runner's per-run tool-call transition channels). They carry
/// just enough context for `app_runtime` to identify which run/path the
/// event belongs to; the runtime decides what UI updates or follow-up
/// requests to issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataEvent {
    /// The live-summary watcher reported a write. The runtime should
    /// (re-)read the live-summary file via [`DataRequest::ReadLiveSummary`]
    /// and update the watchdog idle clock for the active run.
    LiveSummaryChanged,
    /// The runner observed a tool-call lifecycle transition for `window_name`.
    /// The runtime maps it onto the matching watchdog entry.
    ToolCallTransition {
        window_name: String,
        transition: ToolCallTransition,
    },
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
    rx: mpsc::Receiver<()>,
}

impl LiveSummaryEvents {
    /// Wrap a notify receiver. Constructed by [`crate::data::observation`]
    /// when a watcher is built; not part of the public seam since the rx is
    /// an internal detail.
    pub(crate) fn new(rx: mpsc::Receiver<()>) -> Self {
        Self { rx }
    }

    /// Drain every pending watcher signal as a typed [`DataEvent`]. Returns
    /// an empty vector when nothing is queued. Non-blocking.
    pub fn drain(&self) -> Vec<DataEvent> {
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
    /// Best-effort interrupt of the named ACP run with `text` queued as the
    /// next prompt (spec §3.4 watchdog warning).
    InterruptRun { window_name: String, text: String },
    /// Best-effort terminate of the named ACP run (spec §3.5 watchdog kill).
    TerminateRun { window_name: String },
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

/// Dispatch a [`DataRequest`] to the appropriate data primitive. The runtime
/// uses this as its only entry point for side effects so the surface stays
/// small and reviewable.
pub fn dispatch(request: DataRequest) -> DataOutcome {
    match request {
        DataRequest::ProbeLiveSummary { path } => {
            DataOutcome::LiveSummaryProbed(crate::data::observation::probe_live_summary(&path))
        }
        DataRequest::ReadLiveSummary { path } => {
            DataOutcome::LiveSummaryRead(crate::data::observation::read_live_summary(&path))
        }
        DataRequest::DrainLiveSummary { path } => DataOutcome::LiveSummaryDrained(
            crate::data::observation::drain_live_summary_file(&path),
        ),
        DataRequest::ReadPromptBody { path } => {
            DataOutcome::PromptBodyRead(crate::data::observation::read_prompt_body(&path))
        }
        DataRequest::InterruptRun { window_name, text } => DataOutcome::Interrupted(
            crate::data::runner::force_interrupt_run_label(&window_name, text),
        ),
        DataRequest::TerminateRun { window_name } => {
            DataOutcome::Terminated(crate::data::runner::terminate_run_label(&window_name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::tempdir;

    #[test]
    fn dispatch_probe_returns_missing_when_path_absent() {
        let dir = tempdir().expect("tempdir");
        let outcome = dispatch(DataRequest::ProbeLiveSummary {
            path: dir.path().join("nope.txt"),
        });
        assert_eq!(
            outcome,
            DataOutcome::LiveSummaryProbed(LiveSummaryProbe::Missing)
        );
    }

    #[test]
    fn dispatch_read_returns_none_when_path_absent() {
        let dir = tempdir().expect("tempdir");
        let outcome = dispatch(DataRequest::ReadLiveSummary {
            path: dir.path().join("nope.txt"),
        });
        assert_eq!(outcome, DataOutcome::LiveSummaryRead(None));
    }

    #[test]
    fn dispatch_drain_removes_file_after_read() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("live.txt");
        std::fs::write(&path, "draining payload").expect("seed");

        let outcome = dispatch(DataRequest::DrainLiveSummary { path: path.clone() });
        match outcome {
            DataOutcome::LiveSummaryDrained(Some(snapshot)) => {
                assert_eq!(snapshot.content, "draining payload");
                // mtime is whatever the OS reported; just assert it's set.
                let _: SystemTime = snapshot.mtime;
            }
            other => panic!("expected drained snapshot, got {other:?}"),
        }
        assert!(!path.exists(), "drain should remove the live-summary file");
    }

    #[test]
    fn dispatch_read_prompt_returns_none_when_missing() {
        let dir = tempdir().expect("tempdir");
        let outcome = dispatch(DataRequest::ReadPromptBody {
            path: dir.path().join("missing.prompt"),
        });
        assert_eq!(outcome, DataOutcome::PromptBodyRead(None));
    }

    #[test]
    fn dispatch_interrupt_returns_false_when_no_active_run() {
        // No managed ACP run is registered for this window name in tests.
        let outcome = dispatch(DataRequest::InterruptRun {
            window_name: "codexize-events-test-no-such-window".to_string(),
            text: "warn".to_string(),
        });
        assert_eq!(outcome, DataOutcome::Interrupted(false));
    }

    #[test]
    fn dispatch_terminate_returns_false_when_no_active_run() {
        let outcome = dispatch(DataRequest::TerminateRun {
            window_name: "codexize-events-test-no-such-window".to_string(),
        });
        assert_eq!(outcome, DataOutcome::Terminated(false));
    }
}
