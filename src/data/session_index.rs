//! Mtime-cached read model for `sessions/<id>/session.toml` files.
//!
//! The shell scheduler tick and (eventually) the sidebar model both ask
//! "what sessions exist on disk, what phase are they in, are they archived?".
//! Today every caller answers that by walking the sessions directory and
//! calling [`SessionState::load`] for every entry — which parses the full
//! TOML, including fields the scheduler does not need. With N sessions and
//! one tick per UI loop iteration this dominates per-frame cost.
//!
//! [`SessionIndex`] keeps a per-session entry keyed by the last-observed
//! `session.toml` mtime. [`SessionIndex::refresh`] walks the directory,
//! calls `metadata().modified()` on each file, and only reparses entries
//! whose mtime advanced (or that are new since the last refresh). Sessions
//! whose directory disappeared are evicted. Per-session load failures
//! become `ScannedSession::Corrupt` entries so the scheduler's existing
//! corrupt-earlier-session policy keeps working.
//!
//! Tests can observe the load-call count via
//! [`SessionIndex::loader_call_count`] — an instance-local counter so
//! parallel tests do not race a process-global. Existing call sites that
//! still want the one-shot scan continue to use
//! [`crate::data::picker_io::scan_sessions_for_scheduler`], which is now a
//! thin wrapper that builds a fresh index, refreshes once, and snapshots.

use crate::scheduler::{ScannedSession, SchedulerSession};
use crate::state::{Phase, SessionState};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Cached projection of one `session.toml` for the scheduler/sidebar read
/// models. Intentionally narrow: only fields the shell consults during a
/// scheduler tick or sidebar refresh.
#[derive(Debug, Clone)]
pub struct IndexedSession {
    pub session_id: String,
    pub phase: Phase,
    pub archived: bool,
}

#[derive(Debug, Clone)]
enum Entry {
    Loaded {
        mtime: SystemTime,
        indexed: IndexedSession,
    },
    Corrupt {
        mtime: SystemTime,
        error: String,
    },
}

impl Entry {
    fn mtime(&self) -> SystemTime {
        match self {
            Entry::Loaded { mtime, .. } | Entry::Corrupt { mtime, .. } => *mtime,
        }
    }
}

/// Mtime-cached index of `sessions/<id>/session.toml` projections.
///
/// Each call to [`refresh`](Self::refresh) re-walks the sessions directory,
/// reparses only entries whose `session.toml` mtime advanced (or that are
/// new), and evicts entries whose directory disappeared.
pub struct SessionIndex {
    sessions_root: PathBuf,
    entries: BTreeMap<String, Entry>,
    loader_call_count: usize,
}

impl SessionIndex {
    pub fn new(sessions_root: PathBuf) -> Self {
        Self {
            sessions_root,
            entries: BTreeMap::new(),
            loader_call_count: 0,
        }
    }

    /// Number of full `SessionState::load` calls performed by this index
    /// over its lifetime. The shell scheduler tests use this to assert
    /// that a steady-state tick does not full-load every session.
    pub fn loader_call_count(&self) -> usize {
        self.loader_call_count
    }

    /// Walk the sessions directory and reparse only entries whose
    /// `session.toml` mtime advanced or that are new since the last
    /// refresh. Entries whose directory disappeared are evicted.
    ///
    /// `session.toml` files that fail to parse are stored as corrupt
    /// entries so the scheduler can surface them via
    /// [`ScannedSession::Corrupt`].
    pub fn refresh(&mut self) -> Result<()> {
        if !self.sessions_root.exists() {
            fs::create_dir_all(&self.sessions_root)?;
            self.entries.clear();
            return Ok(());
        }
        let mut observed: BTreeSet<String> = BTreeSet::new();
        for dir_entry in fs::read_dir(&self.sessions_root)? {
            let dir_entry = dir_entry?;
            let path = dir_entry.path();
            if !path.is_dir() {
                continue;
            }
            let session_id = match path.file_name().and_then(|n| n.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let toml_path = path.join("session.toml");
            if !toml_path.exists() {
                continue;
            }
            let mtime = fs::metadata(&toml_path)?.modified()?;
            let cached_mtime = self.entries.get(&session_id).map(Entry::mtime);
            // Reparse on first sight or when the on-disk mtime advanced.
            // Equal mtimes treat the cache as authoritative — the file
            // bytes have not changed since the last refresh, so a parse
            // would only re-confirm what we already cached.
            if cached_mtime != Some(mtime) {
                self.loader_call_count += 1;
                match SessionState::load(&session_id) {
                    Ok(state) => {
                        self.entries.insert(
                            session_id.clone(),
                            Entry::Loaded {
                                mtime,
                                indexed: IndexedSession {
                                    session_id: session_id.clone(),
                                    phase: state.current_phase,
                                    archived: state.archived,
                                },
                            },
                        );
                    }
                    Err(err) => {
                        self.entries.insert(
                            session_id.clone(),
                            Entry::Corrupt {
                                mtime,
                                error: format!("{err:#}"),
                            },
                        );
                    }
                }
            }
            observed.insert(session_id);
        }
        self.entries.retain(|id, _| observed.contains(id));
        Ok(())
    }

    /// Project the cache into the scheduler's input shape. Archived
    /// sessions are dropped so the scheduler does not consider them, and
    /// corrupt entries are surfaced explicitly so the existing
    /// corrupt-earlier-session policy applies. Entries are sorted by
    /// session id (creation order) to match
    /// [`crate::data::picker_io::scan_sessions_for_scheduler`].
    pub fn snapshot_for_scheduler(&self) -> Vec<ScannedSession> {
        let mut out: Vec<ScannedSession> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| match entry {
                Entry::Loaded { indexed, .. } if indexed.archived => None,
                Entry::Loaded { indexed, .. } => Some(ScannedSession::Loaded(SchedulerSession {
                    session_id: id.clone(),
                    current_phase: indexed.phase,
                })),
                Entry::Corrupt { error, .. } => Some(ScannedSession::Corrupt {
                    session_id: id.clone(),
                    error: error.clone(),
                }),
            })
            .collect();
        out.sort_by(|a, b| a.session_id().cmp(b.session_id()));
        out
    }

    /// Lookup the cached projection for a session, if it loaded
    /// successfully on the most recent refresh.
    pub fn get(&self, session_id: &str) -> Option<&IndexedSession> {
        match self.entries.get(session_id)? {
            Entry::Loaded { indexed, .. } => Some(indexed),
            Entry::Corrupt { .. } => None,
        }
    }
}

/// One-shot helper used by callers that do not hold a long-lived index
/// (the picker's startup path, retry-allowed gating, tests). Builds a
/// fresh index, refreshes once, and projects it for the scheduler. The
/// shell scheduler tick uses a long-lived index instead.
pub fn snapshot_for_scheduler(sessions_root: &Path) -> Result<Vec<ScannedSession>> {
    let mut index = SessionIndex::new(sessions_root.to_path_buf());
    index.refresh()?;
    Ok(index.snapshot_for_scheduler())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{self, Phase, SessionState, test_fs_lock};
    use serial_test::serial;

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = test_fs_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().expect("tempdir");
        // Safety: tests are serialised by `test_fs_lock`, so the env var is
        // not touched by another thread concurrently.
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", tmp.path());
        }
        let out = f();
        unsafe {
            std::env::remove_var("CODEXIZE_ROOT");
        }
        drop(tmp);
        out
    }

    fn save_session(id: &str, phase: Phase) {
        let mut state = SessionState::new(id.to_string());
        state.current_phase = phase;
        state.save().expect("save session");
    }

    fn sessions_root() -> PathBuf {
        state::codexize_root().join("sessions")
    }

    fn touch_session_toml(id: &str) {
        // Force a real mtime advance by rewriting the file with the same
        // content via `save()`. On filesystems with coarse mtime
        // resolution (HFS+ at 1s) tests sleep before invoking us, so the
        // new mtime is strictly later.
        let state = SessionState::load(id).expect("load for touch");
        state.save().expect("save for touch");
    }

    /// Sleep long enough that the next `save()` produces a strictly later
    /// mtime even on coarse-resolution filesystems. APFS resolves to
    /// nanoseconds; this is belt-and-suspenders for portability.
    fn advance_mtime_clock() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    #[test]
    #[serial]
    fn refresh_loads_every_session_on_first_call() {
        with_temp_root(|| {
            save_session("20260511-080000-000000001", Phase::BrainstormRunning);
            save_session("20260511-090000-000000001", Phase::WaitingToImplement);
            save_session("20260511-100000-000000001", Phase::Done);

            let mut index = SessionIndex::new(sessions_root());
            index.refresh().expect("refresh");

            assert_eq!(index.loader_call_count(), 3);
            let snap = index.snapshot_for_scheduler();
            assert_eq!(snap.len(), 3);
        });
    }

    #[test]
    #[serial]
    fn refresh_reparses_only_changed_entries() {
        with_temp_root(|| {
            save_session("20260511-080000-000000001", Phase::BrainstormRunning);
            save_session("20260511-090000-000000001", Phase::WaitingToImplement);
            save_session("20260511-100000-000000001", Phase::Done);

            let mut index = SessionIndex::new(sessions_root());
            index.refresh().expect("first refresh");
            assert_eq!(index.loader_call_count(), 3);

            advance_mtime_clock();
            touch_session_toml("20260511-090000-000000001");

            index.refresh().expect("second refresh");
            // Only the touched file should trigger a reparse.
            assert_eq!(index.loader_call_count(), 4);
        });
    }

    #[test]
    #[serial]
    fn refresh_is_steady_state_when_nothing_changed() {
        with_temp_root(|| {
            save_session("20260511-080000-000000001", Phase::BrainstormRunning);
            save_session("20260511-090000-000000001", Phase::WaitingToImplement);

            let mut index = SessionIndex::new(sessions_root());
            index.refresh().expect("first refresh");
            let baseline = index.loader_call_count();

            index.refresh().expect("second refresh");
            index.refresh().expect("third refresh");

            assert_eq!(
                index.loader_call_count(),
                baseline,
                "steady-state refresh must not reparse unchanged session.toml files"
            );
        });
    }

    #[test]
    #[serial]
    fn refresh_evicts_deleted_sessions() {
        with_temp_root(|| {
            save_session("20260511-080000-000000001", Phase::BrainstormRunning);
            save_session("20260511-090000-000000001", Phase::WaitingToImplement);

            let mut index = SessionIndex::new(sessions_root());
            index.refresh().expect("first refresh");
            assert_eq!(index.snapshot_for_scheduler().len(), 2);

            std::fs::remove_dir_all(state::session_dir("20260511-080000-000000001"))
                .expect("rm session dir");
            index.refresh().expect("second refresh");

            let snap = index.snapshot_for_scheduler();
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].session_id(), "20260511-090000-000000001");
            assert!(index.get("20260511-080000-000000001").is_none());
        });
    }

    #[test]
    #[serial]
    fn refresh_surfaces_invalid_session_toml_as_corrupt() {
        with_temp_root(|| {
            save_session("20260511-080000-000000001", Phase::BrainstormRunning);
            // Truncate the second session's toml to invalid bytes — load
            // will fail with a parse error.
            let bad_id = "20260511-090000-000000001";
            std::fs::create_dir_all(state::session_dir(bad_id)).expect("mkdir bad session");
            std::fs::write(
                state::session_dir(bad_id).join("session.toml"),
                b"!!! not valid toml",
            )
            .expect("write bad toml");

            let mut index = SessionIndex::new(sessions_root());
            index.refresh().expect("refresh");

            let snap = index.snapshot_for_scheduler();
            assert_eq!(snap.len(), 2);
            let corrupt = snap
                .iter()
                .find(|s| s.session_id() == bad_id)
                .expect("bad session present");
            assert!(matches!(corrupt, ScannedSession::Corrupt { .. }));
            // The corrupt entry is not promoted to a loaded `IndexedSession`.
            assert!(index.get(bad_id).is_none());
        });
    }

    #[test]
    #[serial]
    fn refresh_filters_archived_from_scheduler_snapshot() {
        with_temp_root(|| {
            let mut state = SessionState::new("20260511-080000-000000001".to_string());
            state.current_phase = Phase::Done;
            state.archived = true;
            state.save().expect("save archived");
            save_session("20260511-090000-000000001", Phase::BrainstormRunning);

            let mut index = SessionIndex::new(sessions_root());
            index.refresh().expect("refresh");

            let snap = index.snapshot_for_scheduler();
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].session_id(), "20260511-090000-000000001");
        });
    }
}
