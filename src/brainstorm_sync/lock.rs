//! Non-blocking lock for the brainstorm sync metadata directory.
//!
//! The shared `cache_lock` helper retries for up to 60 seconds before giving
//! up — appropriate for cache writes, but wrong for startup preflight, where
//! a held lock from another codexize process must skip sync immediately
//! rather than block the user. This module wraps the same on-disk lockfile
//! format with a single-attempt acquire and stale-detection break, so two
//! parallel terminals never race a destructive package replacement.

use anyhow::{Context, Result};
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

/// Maximum age of a lockfile before it is considered stale and reclaimable.
/// Matches `cache_lock` so a stale lock written by either implementation
/// is recognized by the other.
const STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome<T> {
    Held(T),
    Skipped,
}

impl<T> LockOutcome<T> {
    pub fn is_skipped(&self) -> bool {
        matches!(self, LockOutcome::Skipped)
    }
}

/// Try to acquire the brainstorm sync lock at `path`. If another live
/// process holds it, return [`LockOutcome::Skipped`] without retrying so the
/// caller can fall through to normal startup. Stale locks (dead PID or
/// expired timestamp) are reclaimed and the work runs.
pub fn try_with_lock<T>(path: &Path, work: impl FnOnce() -> Result<T>) -> Result<LockOutcome<T>> {
    match acquire(path)? {
        AcquireOutcome::Acquired => {}
        AcquireOutcome::Contended => return Ok(LockOutcome::Skipped),
    }
    let result = work();
    let release_result = release(path);
    match (result, release_result) {
        (Ok(value), Ok(())) => Ok(LockOutcome::Held(value)),
        (Ok(_), Err(release_err)) => Err(release_err),
        (Err(work_err), Ok(())) => Err(work_err),
        (Err(work_err), Err(release_err)) => {
            eprintln!(
                "warning: failed to release brainstorm sync lock at {} after closure error: {release_err:#}",
                path.display()
            );
            Err(work_err)
        }
    }
}

enum AcquireOutcome {
    Acquired,
    Contended,
}

fn acquire(path: &Path) -> Result<AcquireOutcome> {
    match try_create(path) {
        Ok(()) => Ok(AcquireOutcome::Acquired),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            if try_break_stale(path) {
                // Try one more time after breaking a stale lock; a second
                // collision means another live process won the race.
                match try_create(path) {
                    Ok(()) => Ok(AcquireOutcome::Acquired),
                    Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                        Ok(AcquireOutcome::Contended)
                    }
                    Err(err) => Err(err).with_context(|| {
                        format!("failed to create lock file at {}", path.display())
                    }),
                }
            } else {
                Ok(AcquireOutcome::Contended)
            }
        }
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
    let contents = format!("{}\n{}\n", std::process::id(), now_secs());
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn try_break_stale(path: &Path) -> bool {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut lines = contents.lines();
    let pid: i32 = match lines.next().and_then(|s| s.parse().ok()) {
        Some(p) => p,
        None => {
            // Malformed contents: drop it.
            let _ = fs::remove_file(path);
            return true;
        }
    };
    let created_at: u64 = lines.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let age_exceeded = now_secs().saturating_sub(created_at) >= STALE_AFTER.as_secs();
    let pid_dead = !is_process_alive(pid);
    if age_exceeded || pid_dead {
        let _ = fs::remove_file(path);
        return true;
    }
    false
}

fn release(path: &Path) -> Result<()> {
    if let Ok(contents) = fs::read_to_string(path)
        && let Some(pid_str) = contents.lines().next()
        && let Ok(pid) = pid_str.parse::<u32>()
        && pid == std::process::id()
    {
        fs::remove_file(path).with_context(|| {
            format!(
                "failed to release brainstorm sync lock at {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn is_process_alive(pid: i32) -> bool {
    use nix::sys::signal;
    use nix::unistd::Pid;
    signal::kill(Pid::from_raw(pid), None).is_ok()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn lock_path(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("metadata.toml.lock")
    }

    #[test]
    fn lock_runs_work_and_releases() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        let outcome = try_with_lock(&path, || Ok(7)).unwrap();
        assert!(matches!(outcome, LockOutcome::Held(7)));
        assert!(!path.exists(), "lock file should be removed after release");
    }

    #[test]
    fn live_holder_causes_skip_without_retry() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        // Forge a lockfile owned by the current process: that PID is
        // guaranteed alive and signal-permitted, so stale-detection cannot
        // reclaim it. A fresh timestamp blocks age-based reclamation too.
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let owner = std::process::id();
        fs::write(&path, format!("{owner}\n{}\n", now_secs())).unwrap();

        let outcome: LockOutcome<()> = try_with_lock(&path, || {
            panic!("work must not run when another process holds the lock")
        })
        .unwrap();
        assert!(outcome.is_skipped());
        // The held lock must remain in place — try_with_lock must not
        // touch a lock it never acquired, even if the recorded PID matches
        // the current process.
        assert!(path.exists());
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with(&format!("{owner}\n")));
    }

    #[test]
    fn stale_pid_is_reclaimed() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // PID that's almost certainly dead.
        fs::write(&path, format!("1999999\n{}\n", now_secs())).unwrap();
        let outcome = try_with_lock(&path, || Ok("ran")).unwrap();
        assert!(matches!(outcome, LockOutcome::Held("ran")));
        assert!(!path.exists());
    }

    #[test]
    fn old_timestamp_with_live_pid_is_reclaimed() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = now_secs() - 120;
        // PID 1 is live, but the lockfile is past STALE_AFTER.
        fs::write(&path, format!("1\n{old}\n")).unwrap();
        let outcome = try_with_lock(&path, || Ok(())).unwrap();
        assert!(matches!(outcome, LockOutcome::Held(())));
    }

    #[test]
    fn malformed_lock_is_reclaimed() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "garbage").unwrap();
        let outcome = try_with_lock(&path, || Ok(42)).unwrap();
        assert!(matches!(outcome, LockOutcome::Held(42)));
    }

    #[test]
    fn work_error_propagates_and_releases_lock() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        let err = try_with_lock(&path, || -> Result<()> { anyhow::bail!("boom") }).unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert!(!path.exists(), "lock file must be released on work error");
    }
}
