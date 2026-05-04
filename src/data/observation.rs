//! Observation primitives for backend-side state changes.
//!
//! Owns the world-facing observation IO so the runtime/UI side stays focused
//! on routing decisions: notify-watcher construction, live-summary metadata
//! probing, content reads, end-of-run drain/remove, and prompt-body reads
//! used to compose watchdog warnings. Each primitive returns a plain typed
//! value or outcome — callers decide what to do with it.

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

/// How long without an mtime advance before a live-summary file is treated
/// as stale and cleared from cache. Mirrors the operator-stated heartbeat
/// expectation (spec §3.7).
pub const LIVE_SUMMARY_STALE_AFTER: Duration = Duration::from_secs(60);

/// Outcome of building a live-summary watcher: either a working
/// (`watcher`, `rx`) pair, a degraded `PollOnly` fallback because the
/// underlying notify backend rejected the path, or `Disabled` when no
/// watcher is needed.
pub enum LiveSummaryWatcher {
    Active {
        watcher: RecommendedWatcher,
        rx: mpsc::Receiver<()>,
    },
    PollOnly {
        reason: String,
    },
    Disabled,
}

/// Snapshot of a live-summary file at a specific mtime. The content is
/// returned verbatim; sanitization/dedup is the caller's responsibility.
#[derive(Debug, Clone)]
pub struct LiveSummarySnapshot {
    pub mtime: SystemTime,
    pub content: String,
}

/// Result of a metadata-only probe of the live-summary path. Callers use
/// this to decide whether a full read is worth doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSummaryProbe {
    /// Path is missing, unreadable, or its mtime cannot be determined.
    Missing,
    /// File exists and was last written more than [`LIVE_SUMMARY_STALE_AFTER`]
    /// ago — treat as stale and clear any cached content.
    Stale,
    /// File exists and is fresh; `mtime` is the last write timestamp.
    Fresh { mtime: SystemTime },
}

/// Resolve and create the directory `notify` will watch for
/// `live_summary_path`.
///
/// Returns the watch root on success, or a human-readable reason matching
/// the prior boundary-error wording when the parent could not be created.
/// Callers fall back to mtime polling on `Err`.
pub fn ensure_live_summary_watch_dir(live_summary_path: &Path) -> Result<PathBuf, String> {
    let watch_path: PathBuf = live_summary_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if let Err(e) = std::fs::create_dir_all(&watch_path) {
        return Err(format!("watcher setup failed: {e}, falling back to poll"));
    }
    Ok(watch_path)
}

/// Build a `notify` watcher that fires on writes to `live_summary_path`.
///
/// Calls [`ensure_live_summary_watch_dir`] first, then installs the watcher.
/// Returns `PollOnly { reason }` for any recoverable error so callers can
/// fall back to mtime polling without aborting the run.
pub fn build_live_summary_watcher(live_summary_path: &Path) -> LiveSummaryWatcher {
    let watch_path = match ensure_live_summary_watch_dir(live_summary_path) {
        Ok(path) => path,
        Err(reason) => return LiveSummaryWatcher::PollOnly { reason },
    };

    let (tx, rx) = mpsc::channel();
    let watched_file = live_summary_path.to_path_buf();
    let watcher_result = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res
                && event.paths.iter().any(|changed| changed == &watched_file)
            {
                let _ = tx.send(());
            }
        },
        Config::default(),
    );
    match watcher_result {
        Ok(mut watcher) => match watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
            Ok(()) => LiveSummaryWatcher::Active { watcher, rx },
            Err(e) => LiveSummaryWatcher::PollOnly {
                reason: format!("watcher setup failed: {e}, falling back to poll"),
            },
        },
        Err(e) => LiveSummaryWatcher::PollOnly {
            reason: format!("watcher init failed: {e}, falling back to poll"),
        },
    }
}

/// Cheap metadata probe of the live-summary path. Performs no content read.
pub fn probe_live_summary(path: &Path) -> LiveSummaryProbe {
    let Ok(meta) = std::fs::metadata(path) else {
        return LiveSummaryProbe::Missing;
    };
    let Ok(mtime) = meta.modified() else {
        return LiveSummaryProbe::Missing;
    };
    let stale = mtime
        .elapsed()
        .map(|d| d > LIVE_SUMMARY_STALE_AFTER)
        .unwrap_or(true);
    if stale {
        LiveSummaryProbe::Stale
    } else {
        LiveSummaryProbe::Fresh { mtime }
    }
}

/// Read the live-summary file along with its mtime. Returns `None` when
/// the file is missing, mtime cannot be determined, the contents have been
/// stale longer than [`LIVE_SUMMARY_STALE_AFTER`], or the read fails.
pub fn read_live_summary(path: &Path) -> Option<LiveSummarySnapshot> {
    let mtime = match probe_live_summary(path) {
        LiveSummaryProbe::Fresh { mtime } => mtime,
        LiveSummaryProbe::Missing | LiveSummaryProbe::Stale => return None,
    };
    let content = std::fs::read_to_string(path).ok()?;
    Some(LiveSummarySnapshot { mtime, content })
}

/// Best-effort final read of the live-summary file followed by removal.
/// Returns the snapshot (verbatim, no staleness gate) when readable; the
/// removal is attempted regardless of read success so the next run starts
/// with a clean slate. Errors on either step are intentionally swallowed —
/// the watchdog/cleanup path must not fail end-of-run finalization.
pub fn drain_live_summary_file(path: &Path) -> Option<LiveSummarySnapshot> {
    let snapshot = match (
        std::fs::read_to_string(path),
        std::fs::metadata(path).and_then(|m| m.modified()),
    ) {
        (Ok(content), Ok(mtime)) => Some(LiveSummarySnapshot { mtime, content }),
        _ => None,
    };
    let _ = std::fs::remove_file(path);
    snapshot
}

/// Read a prompt-body file from disk. Returns `None` when the file is
/// missing or unreadable; the watchdog warning path substitutes a
/// documented fallback string in that case (spec §3.4).
pub fn read_prompt_body(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
