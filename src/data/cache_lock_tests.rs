use super::*;
use crate::data::process_probe::tests::MockProbe;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

const HOST: &str = "test-host";

fn lock_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("test.lock")
}

fn write_lock(path: &Path, pid: i32, hostname: &str, started_at: DateTime<Utc>) {
    let body = toml::to_string(&CacheLockContents {
        pid,
        hostname: hostname.to_string(),
        started_at,
    })
    .unwrap();
    fs::write(path, body).unwrap();
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
    // PID 1_999_999 is almost certainly dead — the production probe will
    // confirm and the lock is reclaimed. The lock must record the *real*
    // hostname, otherwise `with_lock` (which uses the production hostname)
    // would treat it as a cross-host lock and refuse to break it.
    write_lock(&path, 1_999_999, &hostname(), Utc::now());
    let result = with_lock(&path, || Ok("ok")).unwrap();
    assert_eq!(result, "ok");
}

#[test]
fn recycled_pid_with_mismatched_start_time_is_broken_via_probe() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let recycled_pid = 9001;
    let recorded = Utc::now() - chrono::Duration::hours(2);
    write_lock(&path, recycled_pid, HOST, recorded);
    // Live but unrelated process: alive, mismatched start time, non-codexize
    // exec → must be reclaimed.
    let probe = MockProbe::live(recycled_pid, Utc::now(), PathBuf::from("/bin/sleep"));
    assert!(
        try_break_stale_with(&path, &probe, HOST),
        "recycled-pid lock must be broken"
    );
    assert!(!path.exists());
}

#[test]
fn live_holder_with_matching_start_time_is_not_broken() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let pid = 9001;
    let recorded = Utc::now();
    write_lock(&path, pid, HOST, recorded);
    let probe = MockProbe::live(pid, recorded, PathBuf::from("/usr/bin/codexize"));
    assert!(
        !try_break_stale_with(&path, &probe, HOST),
        "live same-host holder must not be broken"
    );
    // The live holder's lock must remain untouched.
    assert!(path.exists());
}

#[test]
fn cross_host_lock_is_not_broken_or_probed() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let other_pid = 9001;
    write_lock(&path, other_pid, "other-host", Utc::now());
    // Even if the local probe would say "alive", the cross-host branch must
    // refuse to break the lock without consulting the probe.
    let probe = MockProbe::dead();
    assert!(
        !try_break_stale_with(&path, &probe, HOST),
        "cross-host lock must never be broken"
    );
    assert!(path.exists(), "cross-host lock file must remain intact");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_lock_serializes_access() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let p = path.clone();
            let c = Arc::clone(&counter);
            let b = Arc::clone(&barrier);
            tokio::task::spawn_blocking(move || {
                crate::data::async_bridge::block_on_io(b.wait());
                with_lock(&p, || {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    crate::data::async_bridge::sleep_blocking(Duration::from_millis(50));
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();

    for h in handles {
        h.await.unwrap();
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
fn try_acquire_returns_false_when_live_pid_holds_lock() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // Use a synthetic live holder via the mock probe so the test does not
    // depend on the parent test process's own start-time being within
    // tolerance — that would be flaky in slow CI.
    let holder_pid = 12_345;
    let holder_started = Utc::now();
    write_lock(&path, holder_pid, HOST, holder_started);
    let probe = MockProbe::live(
        holder_pid,
        holder_started,
        PathBuf::from("/usr/bin/codexize"),
    );
    let elected =
        try_acquire_with(&path, &probe, HOST.to_string()).expect("try_acquire should not error");
    assert!(!elected, "live-PID lock must yield follower (Ok(false))");
    let raw = fs::read_to_string(&path).unwrap();
    let parsed: CacheLockContents = toml::from_str(&raw).unwrap();
    assert_eq!(
        parsed.pid, holder_pid,
        "live holder's lock contents must be preserved"
    );
}

#[test]
fn try_acquire_succeeds_when_unheld() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    let elected = try_acquire(&path).expect("try_acquire should not error");
    assert!(elected, "unheld lock must elect caller as publisher");
    assert!(path.exists(), "lock file must be created on success");
    release(&path).unwrap();
    assert!(!path.exists(), "release must remove our lock");
}

#[test]
fn try_acquire_breaks_stale_lock_and_retries() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // PID 1_999_999 is almost certainly dead — stale-break kicks in, retry
    // succeeds.
    write_lock(&path, 1_999_999, HOST, Utc::now());
    let elected = try_acquire_with(&path, &MockProbe::dead(), HOST.to_string())
        .expect("try_acquire should not error");
    assert!(elected, "stale lock must be broken and reacquired");
    release(&path).unwrap();
}

#[test]
fn try_acquire_returns_false_for_cross_host_lock() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    write_lock(&path, 12_345, "other-host", Utc::now());
    let elected = try_acquire_with(&path, &MockProbe::dead(), HOST.to_string())
        .expect("try_acquire should not error");
    assert!(
        !elected,
        "cross-host lock must yield follower (no remote probe)"
    );
    let raw = fs::read_to_string(&path).unwrap();
    let parsed: CacheLockContents = toml::from_str(&raw).unwrap();
    assert_eq!(
        parsed.hostname, "other-host",
        "cross-host lock must be untouched"
    );
}

#[test]
fn release_only_removes_own_lock() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    write_lock(&path, 999_999, HOST, Utc::now());
    release(&path).unwrap();
    assert!(path.exists(), "should not remove another process's lock");
}

#[test]
fn release_propagates_remove_failure() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // Stage a lock owned by us so the read+pid checks pass.
    write_lock(&path, std::process::id() as i32, HOST, Utc::now());

    // Lock the parent directory so `fs::remove_file` cannot unlink the
    // lockfile, forcing the inner remove to fail.
    let original = fs::metadata(dir.path()).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_mode(0o555);
    fs::set_permissions(dir.path(), readonly).unwrap();

    let result = release(&path);

    // Restore so TempDir can clean up.
    fs::set_permissions(dir.path(), original).unwrap();

    let err = result.expect_err("release must surface remove_file errors");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed to release lock"),
        "missing context in error: {msg}"
    );
}
