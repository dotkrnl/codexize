use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::io::{self, ErrorKind};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::warn;

use crate::data::process_probe::{ProcessProbe, RealProbe, hostname, same_host_holder_alive};

const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// On-disk schema for the cache lock. Mirrors `app_lock::AppLockContents`
/// minus `project_root` because the cache lock is per-cache-directory, not
/// per-project. Sharing the same `pid + hostname + started_at` shape lets
/// both locks consult the same `process_probe::same_host_holder_alive` rule
/// for stale-vs-live decisions, so a crashed `codexize` recovers from both
/// in the same way (spec.md §"Process lock", §"Failure and edge behavior").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheLockContents {
    pub pid: i32,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
}

/// Execute `f` while holding an exclusive lockfile at `path`.
///
/// Acquisition uses `O_CREAT | O_EXCL` for atomicity. If the lock is held by
/// a live same-host process, retries until `LOCK_TIMEOUT`. Cross-host locks
/// are treated as live (no remote probe) so the caller times out rather than
/// stomping a peer's lock. Same-host stale locks (dead pid OR start-time /
/// executable mismatch defeating PID recycling) are reclaimed automatically.
///
/// Known limitation: `O_CREAT|O_EXCL` may be unreliable on NFS/SMB mounts;
/// `~/.codexize/` is assumed to reside on a local filesystem.
pub fn with_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    acquire(path)?;
    let work = f();
    let release_result = release(path);
    match (work, release_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(release_err)) => Err(release_err),
        (Err(work_err), Ok(())) => Err(work_err),
        (Err(work_err), Err(release_err)) => {
            // TUI runs must not print to stderr; surface this via tracing so it
            // lands in the session diagnostics log when enabled.
            warn!(
                lock_path = %path.display(),
                error = %release_err,
                "failed to release cache lock after closure error"
            );
            Err(work_err)
        }
    }
}

fn acquire(path: &Path) -> Result<()> {
    acquire_with(path, &RealProbe, hostname())
}

fn acquire_with(path: &Path, probe: &dyn ProcessProbe, self_host: String) -> Result<()> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match try_create(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                if try_break_stale_with(path, probe, &self_host) {
                    continue;
                }
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out waiting for lock at {}", path.display());
                }
                crate::data::async_bridge::sleep_blocking(POLL_INTERVAL);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to create lock file at {}", path.display()));
            }
        }
    }
}

/// Non-blocking publisher election. Attempts a single `O_CREAT|O_EXCL`; on
/// `AlreadyExists` it evaluates the holder via `try_break_stale` and, if the
/// lock was stale, retries exactly once. Returns `Ok(true)` when the caller
/// now owns the lock and must eventually call [`release`]; `Ok(false)` when
/// the lock is held by a live same-host pid (follower mode), held cross-host
/// (treated as held — never broken without an operator), or when a racing
/// peer grabbed the lock between the stale-break and the retry. I/O errors
/// other than `AlreadyExists` propagate.
pub fn try_acquire(path: &Path) -> Result<bool> {
    try_acquire_with(path, &RealProbe, hostname())
}

fn try_acquire_with(path: &Path, probe: &dyn ProcessProbe, self_host: String) -> Result<bool> {
    match try_create(path) {
        Ok(()) => return Ok(true),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to create lock file at {}", path.display()));
        }
    }
    if !try_break_stale_with(path, probe, &self_host) {
        // Holder is a live same-host pid OR a cross-host pid we won't probe
        // — fall back to follower mode in both cases.
        return Ok(false);
    }
    match try_create(path) {
        Ok(()) => Ok(true),
        // A peer raced us between break and retry; spec §Roles says fall back
        // to follower rather than looping.
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("failed to create lock file at {}", path.display()))
        }
    }
}

fn try_create(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = CacheLockContents {
        pid: std::process::id() as i32,
        hostname: hostname(),
        started_at: Utc::now(),
    };
    let serialized = toml::to_string(&contents).map_err(io::Error::other)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(serialized.as_bytes())?;
    Ok(())
}

/// Returns `true` if the stale lock was removed and the caller should retry.
fn try_break_stale_with(path: &Path, probe: &dyn ProcessProbe, self_host: &str) -> bool {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let parsed = match toml::from_str::<CacheLockContents>(&contents) {
        Ok(c) => c,
        Err(_) => {
            // Malformed (including legacy `pid\ntimestamp\n` records left by
            // older builds) → treat as stale and let the caller retry the
            // O_EXCL create.
            let _ = fs::remove_file(path);
            warn!(
                event = "cache_stale_lock_broken",
                lock_path = %path.display(),
                reason = "malformed",
                "broke stale cache lock"
            );
            return true;
        }
    };
    if parsed.hostname != self_host {
        // Different machine; never probe or mutate cross-host holders.
        return false;
    }
    if same_host_holder_alive(parsed.pid, parsed.started_at, probe) {
        return false;
    }
    let _ = fs::remove_file(path);
    warn!(
        event = "cache_stale_lock_broken",
        lock_path = %path.display(),
        holder_pid = parsed.pid,
        "broke stale cache lock"
    );
    true
}

/// Release a lock previously obtained via [`try_acquire`] (or the internal
/// [`acquire`] used by [`with_lock`]). The PID check ensures a process never
/// removes another process's lockfile.
pub fn release(path: &Path) -> Result<()> {
    if let Ok(contents) = fs::read_to_string(path)
        && let Ok(parsed) = toml::from_str::<CacheLockContents>(&contents)
        && parsed.pid == std::process::id() as i32
    {
        fs::remove_file(path)
            .with_context(|| format!("failed to release lock at {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "cache_lock_tests.rs"]
mod tests;
