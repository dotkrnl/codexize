use super::*;
use std::sync::Arc;
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
fn release_only_removes_own_lock() {
    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // Write a lock for a different PID
    fs::write(&path, "999999\n0\n").unwrap();
    release(&path).unwrap();
    assert!(path.exists(), "should not remove another process's lock");
}

#[test]
fn release_propagates_remove_failure() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = lock_path(&dir);
    // Stage a lock owned by us so the read+pid checks pass.
    fs::write(&path, format!("{}\n{}\n", std::process::id(), now_secs())).unwrap();

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
