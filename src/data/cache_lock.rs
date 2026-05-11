use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::io::{self, ErrorKind};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::warn;
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Execute `f` while holding an exclusive PID-based lockfile at `path`.
///
/// Acquisition uses `O_CREAT | O_EXCL` for atomicity. If the lock is held by
/// another live process, retries until `LOCK_TIMEOUT`. Stale locks (dead PID
/// or older than 60 s) are automatically removed.
///
/// Known limitation: `O_CREAT|O_EXCL` may be unreliable on NFS/SMB mounts;
/// `~/.codexize/` is assumed to reside on a local filesystem.
pub fn with_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    acquire(path)?;
    let work = f();
    let release = release(path);
    match (work, release) {
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
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match try_create(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                if try_break_stale(path) {
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
/// `AlreadyExists` it evaluates the holder via [`try_break_stale`] and, if the
/// lock was stale, retries exactly once. Returns `Ok(true)` when the caller
/// now owns the lock and must eventually call [`release`]; `Ok(false)` when
/// the lock is held by a live PID (follower mode) or a racing peer grabbed
/// the lock between the stale-break and the retry. I/O errors other than
/// `AlreadyExists` propagate.
pub fn try_acquire(path: &Path) -> Result<bool> {
    match try_create(path) {
        Ok(()) => return Ok(true),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to create lock file at {}", path.display()));
        }
    }
    if !try_break_stale(path) {
        // Holder is a live PID under the 60 s age threshold — follower.
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
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    let contents = format!("{}\n{}\n", std::process::id(), now_secs(),);
    file.write_all(contents.as_bytes())?;
    Ok(())
}
/// Returns `true` if the stale lock was removed and the caller should retry.
fn try_break_stale(path: &Path) -> bool {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut lines = contents.lines();
    let pid: i32 = match lines.next().and_then(|s| s.parse().ok()) {
        Some(p) => p,
        None => {
            let _ = fs::remove_file(path);
            return true;
        }
    };
    let created_at: u64 = lines.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let age_exceeded = now_secs().saturating_sub(created_at) >= LOCK_TIMEOUT.as_secs();
    let pid_dead = !is_process_alive(pid);
    if age_exceeded || pid_dead {
        let _ = fs::remove_file(path);
        warn!(
            event = "cache_stale_lock_broken",
            lock_path = %path.display(),
            holder_pid = pid,
            age_secs = now_secs().saturating_sub(created_at),
            pid_dead,
            "broke stale cache lock"
        );
        return true;
    }
    false
}
/// Release a lock previously obtained via [`try_acquire`] (or the internal
/// [`acquire`] used by [`with_lock`]). The PID check ensures a process never
/// removes another process's lockfile.
pub fn release(path: &Path) -> Result<()> {
    if let Ok(contents) = fs::read_to_string(path)
        && let Some(pid_str) = contents.lines().next()
        && let Ok(pid) = pid_str.parse::<u32>()
        && pid == std::process::id()
    {
        fs::remove_file(path)
            .with_context(|| format!("failed to release lock at {}", path.display()))?;
    }
    Ok(())
}
fn is_process_alive(pid: i32) -> bool {
    use nix::sys::signal;
    use nix::unistd::Pid;
    process_alive_from_kill_result(signal::kill(Pid::from_raw(pid), None))
}
fn process_alive_from_kill_result(result: Result<(), nix::errno::Errno>) -> bool {
    use nix::errno::Errno;
    matches!(result, Ok(()) | Err(Errno::EPERM))
}
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[cfg(test)]
#[path = "cache_lock_tests.rs"]
mod tests;
