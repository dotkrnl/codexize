//! Observation primitives for backend-side state changes.
//!
//! For now this module owns the live-summary `notify` watcher construction.
//! The orchestrator wiring (which path to watch, how to refocus, when to
//! emit Brief messages) stays under `app::observation` until the runtime
//! seam extraction in a later slice; the data layer just reports facts and
//! produces typed handles.

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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
