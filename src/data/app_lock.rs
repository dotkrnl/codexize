//! Project-level single-process gate (`<project>/.codexize/app.lock`).
//!
//! Spec contract (see spec.md §"Process lock", §"Failure and edge behavior",
//! AC-2):
//!
//! - On startup, write a TOML file with `pid`, `hostname`, `started_at`
//!   (RFC3339 UTC), and absolute `project_root`.
//! - If a same-host lock's pid is alive AND the live process plausibly matches
//!   the recorded start time / executable, refuse with the pinned message.
//! - If a same-host lock's pid is dead OR start-time/executable mismatches
//!   (recycled pid), replace the stale lock and proceed.
//! - If the lock's hostname differs, refuse with the host-and-pid pinned
//!   message and never mutate or remote-probe the lock.
//! - On clean shutdown, remove the lock; failure to remove logs a warning
//!   only — never mutates session state.
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::warn;

use crate::data::process_probe::{ProcessProbe, RealProbe, hostname, same_host_holder_alive};

/// Pinned operator-facing message for the same-host live-owner case. Tests
/// assert against this constant so a future copy edit is caught at compile
/// time.
pub const SAME_HOST_REFUSAL_MESSAGE: &str =
    "codexize is already running for this project; exit that process before starting another one.";

/// Default lock filename inside the project's `.codexize/` directory.
pub const APP_LOCK_FILENAME: &str = "app.lock";

/// On-disk schema for `app.lock`. Field order is preserved by `toml::to_string`
/// so operators eyeballing the file see them in the spec's listed order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppLockContents {
    pub pid: i32,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
    pub project_root: PathBuf,
}

#[derive(Debug, Error)]
pub enum AcquireError {
    #[error("{}", SAME_HOST_REFUSAL_MESSAGE)]
    AlreadyRunningSameHost,
    #[error(
        "codexize lock owned by host {hostname} (pid {pid}); remove .codexize/app.lock manually if that host is offline."
    )]
    OwnedByOtherHost { hostname: String, pid: i32 },
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

/// RAII handle for a held lock. Drops remove the file (best-effort, with a
/// warning on failure); explicit [`AppLockGuard::release`] surfaces the
/// removal error to the caller.
#[derive(Debug)]
pub struct AppLockGuard {
    path: PathBuf,
    pid: i32,
    released: bool,
}

impl AppLockGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the lock file and consume the guard. Returns the underlying
    /// IO error so the caller can surface it (typically as a non-fatal
    /// warning per the spec's "do not mutate sessions" rule).
    pub fn release(mut self) -> Result<()> {
        self.released = true;
        release(&self.path, self.pid)
    }
}

impl Drop for AppLockGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Err(err) = release(&self.path, self.pid) {
            warn!(
                event = "app_lock_release_failed",
                lock_path = %self.path.display(),
                error = %err,
                "failed to remove app.lock on drop; sessions left untouched"
            );
        }
    }
}

/// Acquire the project's app lock with production defaults (real process
/// probe, system hostname, current pid, current UTC time).
pub fn acquire(lock_path: &Path, project_root: &Path) -> Result<AppLockGuard, AcquireError> {
    acquire_with(
        lock_path,
        project_root,
        &RealProbe,
        hostname(),
        std::process::id() as i32,
        Utc::now(),
    )
}

/// Test-friendly acquire that takes the probe, hostname, pid, and timestamp
/// explicitly. Production callers go through [`acquire`].
pub fn acquire_with(
    lock_path: &Path,
    project_root: &Path,
    probe: &dyn ProcessProbe,
    self_host: String,
    self_pid: i32,
    self_started_at: DateTime<Utc>,
) -> Result<AppLockGuard, AcquireError> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create lock dir at {}", parent.display()))
            .map_err(AcquireError::Io)?;
    }
    let contents = AppLockContents {
        pid: self_pid,
        hostname: self_host.clone(),
        started_at: self_started_at,
        project_root: project_root.to_path_buf(),
    };
    // Two-pass: try O_CREAT|O_EXCL; on AlreadyExists, decide refusal vs
    // stale-replace; if we replaced, retry exactly once. A peer that races
    // us between break and retry collapses back into the existence-check
    // branch on the next iteration so we still surface a deterministic
    // refusal rather than overwriting their lock.
    for _ in 0..2 {
        match try_create(lock_path, &contents) {
            Ok(()) => {
                return Ok(AppLockGuard {
                    path: lock_path.to_path_buf(),
                    pid: self_pid,
                    released: false,
                });
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                let existing = match read_existing(lock_path) {
                    Ok(Some(c)) => c,
                    Ok(None) => continue, // file vanished or was malformed; retry create
                    Err(io_err) => return Err(AcquireError::Io(io_err)),
                };
                if existing.hostname != self_host {
                    return Err(AcquireError::OwnedByOtherHost {
                        hostname: existing.hostname,
                        pid: existing.pid,
                    });
                }
                if same_host_holder_alive(existing.pid, existing.started_at, probe) {
                    return Err(AcquireError::AlreadyRunningSameHost);
                }
                warn!(
                    event = "app_stale_lock_replaced",
                    lock_path = %lock_path.display(),
                    holder_pid = existing.pid,
                    "replacing stale app.lock"
                );
                // Best-effort remove. If a peer raced and removed it first,
                // the next try_create either succeeds (we win) or sees a
                // fresh lock and re-evaluates from the top.
                if let Err(rm_err) = fs::remove_file(lock_path)
                    && rm_err.kind() != ErrorKind::NotFound
                {
                    return Err(AcquireError::Io(anyhow::Error::from(rm_err).context(
                        format!("failed to remove stale lock at {}", lock_path.display()),
                    )));
                }
            }
            Err(err) => {
                return Err(AcquireError::Io(anyhow::Error::from(err).context(format!(
                    "failed to create lock file at {}",
                    lock_path.display()
                ))));
            }
        }
    }
    // Two passes saw a live peer or repeated races; treat as same-host live
    // refusal — safer than looping forever on a flapping race.
    Err(AcquireError::AlreadyRunningSameHost)
}

/// Read+parse the lock file. `Ok(None)` covers both "file missing" and
/// "file is malformed" — the latter is unlinked so the caller can retry the
/// O_EXCL create. IO errors (other than NotFound) propagate.
fn read_existing(path: &Path) -> Result<Option<AppLockContents>, anyhow::Error> {
    match fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<AppLockContents>(&contents) {
            Ok(c) => Ok(Some(c)),
            Err(err) => {
                warn!(
                    event = "app_lock_malformed",
                    lock_path = %path.display(),
                    error = %err,
                    "discarding malformed app.lock"
                );
                let _ = fs::remove_file(path);
                Ok(None)
            }
        },
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::from(err))
            .with_context(|| format!("failed to read lock at {}", path.display())),
    }
}

fn try_create(path: &Path, contents: &AppLockContents) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = toml::to_string(contents).map_err(io::Error::other)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(serialized.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Internal release used by both explicit [`AppLockGuard::release`] and the
/// `Drop` path. Removes the file only when its current contents identify our
/// pid — defends against the case where a stale-break replaced our lock with
/// another process's.
fn release(path: &Path, our_pid: i32) -> Result<()> {
    let existing = match read_existing(path) {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(err) => return Err(err),
    };
    if existing.pid != our_pid {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("failed to remove lock at {}", path.display()))
}

#[cfg(test)]
#[path = "app_lock_tests.rs"]
mod tests;
