//! Filesystem watcher and 60-second mtime poller for the home-shared cache.
//!
//! Every TUI instance runs a single [`CacheWatcher`] for the lifetime of the
//! App. When another instance (the publisher) atomically renames
//! `models.json` into place under `<cache_root>`, the watcher fires and the
//! App drops the new content into its model strip without restart or
//! network round-trip.
//!
//! Two independent triggers feed the same reload path:
//!
//! 1. `notify`-crate filesystem watcher on the cache directory, so callers
//!    see sub-2-second latency on every supported platform.
//! 2. 60-second mtime poll as a belt-and-suspenders fallback for events
//!    that the kernel-side notifier coalesced or dropped (macOS FSEvents
//!    bursts, Linux inotify exhaustion, sandboxed file systems).
//!
//! Both triggers funnel through the same "did the file mtime actually
//! advance since I last looked?" predicate, so a single atomic publish
//! never produces two reloads — that satisfies the spec §Follower
//! live-update requirement that the watcher and poller share a debounced
//! reload path.

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;

/// Belt-and-suspenders mtime poll interval. The notify watcher handles
/// the live-update load on every supported platform; the poll only fires
/// for events the kernel-side notifier dropped or coalesced.
pub const CACHE_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Filesystem name of the cache file the watcher tracks. Kept private to
/// the module — callers pass in the directory and the watcher resolves the
/// `models.json` path internally so the file name is not duplicated at
/// every call site.
const CACHE_FILE_NAME: &str = "models.json";

/// Outcome of [`CacheWatcher::start`]. The watcher always returns a usable
/// state — even when the notify backend rejects the path, the 60-second
/// poll keeps the cache fresh. `PollOnly` carries a human-readable
/// `reason` so the App can surface a boundary log.
pub enum CacheWatcherOutcome {
    /// notify watcher installed; both triggers feed the reload predicate.
    Active(CacheWatcher),
    /// notify watcher failed to install; only the 60-second poll is live.
    PollOnly {
        reason: String,
        watcher: CacheWatcher,
    },
}

/// Watches a cache directory for external publishes and exposes a single
/// debounced "should the App reload the cache now?" signal.
///
/// Holds the underlying `notify::RecommendedWatcher` so its lifetime is
/// pinned to the App; dropping the `CacheWatcher` tears the watcher down.
pub struct CacheWatcher {
    dir: PathBuf,
    file_path: PathBuf,
    /// Kept alive for the duration of the watcher; the closure inside
    /// owns the sender half of `rx`. `None` when the notify backend
    /// refused to install — the 60-second poll still runs in that case.
    _watcher: Option<RecommendedWatcher>,
    rx: Option<mpsc::UnboundedReceiver<()>>,
    last_mtime: Option<SystemTime>,
    next_poll_after: Instant,
}

impl CacheWatcher {
    /// Build a watcher rooted at `dir`. `initial_mtime` is the mtime the
    /// App already loaded at startup; the first reload only fires once a
    /// *new* publish lands. Pass `None` if the App has not loaded the
    /// cache yet — the next mtime advance will trigger the initial load.
    ///
    /// Returns [`CacheWatcherOutcome`] rather than `Self` because the
    /// notify backend may refuse to install (filesystem unsupported,
    /// inotify limits, sandboxed paths) and callers still get a poll-only
    /// watcher in the degraded case.
    pub fn start(dir: &Path, initial_mtime: Option<SystemTime>) -> CacheWatcherOutcome {
        let dir_owned = dir.to_path_buf();
        let file_path = dir_owned.join(CACHE_FILE_NAME);
        let next_poll_after = Instant::now() + CACHE_POLL_INTERVAL;
        // Ensure the cache directory exists so `notify` has something to
        // attach to. A follower can start before any publisher has run,
        // in which case the dir is genuinely absent on disk.
        if let Err(e) = std::fs::create_dir_all(&dir_owned) {
            return CacheWatcherOutcome::PollOnly {
                reason: format!("cache watcher setup failed: {e}, falling back to poll"),
                watcher: CacheWatcher {
                    dir: dir_owned,
                    file_path,
                    _watcher: None,
                    rx: None,
                    last_mtime: initial_mtime,
                    next_poll_after,
                },
            };
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let watched_file = file_path.clone();
        let watcher_result = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res
                    && event.paths.iter().any(|p| p == &watched_file)
                {
                    let _ = tx.send(());
                }
            },
            Config::default(),
        );
        let (notify_watcher, rx_opt, reason) = match watcher_result {
            Ok(mut w) => match w.watch(&dir_owned, RecursiveMode::NonRecursive) {
                Ok(()) => (Some(w), Some(rx), None),
                Err(e) => (
                    None,
                    None,
                    Some(format!(
                        "cache watcher setup failed: {e}, falling back to poll"
                    )),
                ),
            },
            Err(e) => (
                None,
                None,
                Some(format!(
                    "cache watcher init failed: {e}, falling back to poll"
                )),
            ),
        };
        let cache_watcher = CacheWatcher {
            dir: dir_owned,
            file_path,
            _watcher: notify_watcher,
            rx: rx_opt,
            last_mtime: initial_mtime,
            next_poll_after,
        };
        match reason {
            None => CacheWatcherOutcome::Active(cache_watcher),
            Some(reason) => CacheWatcherOutcome::PollOnly {
                reason,
                watcher: cache_watcher,
            },
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.dir
    }

    pub fn cache_file_path(&self) -> &Path {
        &self.file_path
    }

    /// Drain pending notify events and run the 60-second poll. Returns
    /// `true` exactly once per external publish — both triggers funnel
    /// through the same mtime comparison so a single atomic rename never
    /// produces two reloads. Returns `false` when nothing changed.
    pub fn poll(&mut self) -> bool {
        let now = Instant::now();
        let watcher_fired = self
            .rx
            .as_mut()
            .map(|rx| {
                let mut any = false;
                while rx.try_recv().is_ok() {
                    any = true;
                }
                any
            })
            .unwrap_or(false);
        let poll_due = now >= self.next_poll_after;
        if !watcher_fired && !poll_due {
            return false;
        }
        if poll_due {
            self.next_poll_after = now + CACHE_POLL_INTERVAL;
        }
        let Some(current_mtime) = current_mtime(&self.file_path) else {
            // File missing or unreadable: nothing to load. Leave
            // `last_mtime` alone so a later publish is still detected.
            return false;
        };
        let changed = match self.last_mtime {
            None => true,
            Some(prev) => current_mtime > prev,
        };
        if changed {
            self.last_mtime = Some(current_mtime);
            true
        } else {
            false
        }
    }
}

/// Cheap mtime probe used by the App at startup to seed
/// `CacheWatcher::start`'s `initial_mtime` argument. Returns `None` when
/// the cache file is missing or its metadata is unreadable.
pub fn current_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;
