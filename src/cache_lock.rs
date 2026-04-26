use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::io::{self, ErrorKind};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

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
    let result = f();
    release(path);
    result
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
                thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to create lock file at {}", path.display()));
            }
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
        return true;
    }
    false
}

fn release(path: &Path) {
    if let Ok(contents) = fs::read_to_string(path)
        && let Some(pid_str) = contents.lines().next()
        && let Ok(pid) = pid_str.parse::<u32>()
        && pid == std::process::id()
    {
        let _ = fs::remove_file(path);
    }
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
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn lock_path(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("test.lock")
    }

    #[test]
    fn lock_acquire_and_release() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        let result = with_lock(&path, || Ok(42)).unwrap();
        assert_eq!(result, 42);
        assert!(!path.exists(), "lock file should be removed after release");
    }

    #[test]
    fn stale_lock_from_dead_pid_is_broken() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        // Write a lock owned by PID 1_999_999 (almost certainly dead)
        fs::write(&path, format!("1999999\n{}\n", now_secs())).unwrap();
        let result = with_lock(&path, || Ok("ok")).unwrap();
        assert_eq!(result, "ok");
    }

    #[test]
    fn stale_lock_from_old_timestamp_is_broken() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        // Write a lock owned by PID 1 (init, always alive) but old enough
        let old_time = now_secs() - 120;
        fs::write(&path, format!("1\n{old_time}\n")).unwrap();
        let result = with_lock(&path, || Ok("ok")).unwrap();
        assert_eq!(result, "ok");
    }

    #[test]
    fn concurrent_lock_serializes_access() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let p = path.clone();
                let c = Arc::clone(&counter);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    with_lock(&p, || {
                        c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(50));
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn malformed_lock_file_is_broken() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        fs::write(&path, "not-a-pid\n").unwrap();
        let result = with_lock(&path, || Ok(1)).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn release_only_removes_own_lock() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        // Write a lock for a different PID
        fs::write(&path, "999999\n0\n").unwrap();
        release(&path);
        assert!(path.exists(), "should not remove another process's lock");
    }
}
