use super::*;
use crate::data::process_probe::tests::MockProbe;
use std::path::PathBuf;
use tempfile::TempDir;

const HOST: &str = "test-host";
const SELF_PID: i32 = 4242;

fn lock_path(dir: &TempDir) -> PathBuf {
    dir.path().join(".codexize").join("app.lock")
}

fn project_root(dir: &TempDir) -> PathBuf {
    dir.path().to_path_buf()
}

fn acquire_default(
    dir: &TempDir,
    probe: &dyn ProcessProbe,
    started_at: DateTime<Utc>,
) -> Result<AppLockGuard, AcquireError> {
    acquire_with(
        &lock_path(dir),
        &project_root(dir),
        probe,
        HOST.to_string(),
        SELF_PID,
        started_at,
    )
}

#[test]
fn writes_lock_with_required_fields_when_no_lock_exists() {
    let dir = TempDir::new().unwrap();
    let now = Utc::now();
    let guard = acquire_default(&dir, &MockProbe::dead(), now).expect("acquire");
    let raw = fs::read_to_string(guard.path()).unwrap();
    let parsed: AppLockContents = toml::from_str(&raw).expect("valid TOML");
    assert_eq!(parsed.pid, SELF_PID);
    assert_eq!(parsed.hostname, HOST);
    // RFC3339 round-trip preserves the timestamp to second-level precision;
    // assert seconds equality to avoid sub-second formatting drift.
    assert_eq!(parsed.started_at.timestamp(), now.timestamp());
    assert_eq!(parsed.project_root, project_root(&dir));
    drop(guard);
    assert!(!lock_path(&dir).exists(), "drop must remove the lock");
}

#[test]
fn refuses_when_same_host_holder_is_alive_with_matching_start_time() {
    let dir = TempDir::new().unwrap();
    let holder_pid = 9001;
    let holder_started = Utc::now();
    // Pre-write a lock owned by the live holder.
    fs::create_dir_all(lock_path(&dir).parent().unwrap()).unwrap();
    fs::write(
        lock_path(&dir),
        toml::to_string(&AppLockContents {
            pid: holder_pid,
            hostname: HOST.to_string(),
            started_at: holder_started,
            project_root: project_root(&dir),
        })
        .unwrap(),
    )
    .unwrap();
    let probe = MockProbe::live(
        holder_pid,
        holder_started,
        PathBuf::from("/usr/bin/codexize"),
    );
    let err = acquire_default(&dir, &probe, Utc::now()).expect_err("must refuse");
    match err {
        AcquireError::AlreadyRunningSameHost => {}
        other => panic!("expected AlreadyRunningSameHost, got {other:?}"),
    }
    // Refusal must not mutate the lock file.
    let raw_after = fs::read_to_string(lock_path(&dir)).unwrap();
    let still: AppLockContents = toml::from_str(&raw_after).unwrap();
    assert_eq!(still.pid, holder_pid);
    // The error string must match the spec-pinned message exactly.
    assert_eq!(
        AcquireError::AlreadyRunningSameHost.to_string(),
        SAME_HOST_REFUSAL_MESSAGE
    );
}

#[test]
fn replaces_stale_same_host_lock_when_pid_is_dead() {
    let dir = TempDir::new().unwrap();
    let dead_pid = 9001;
    fs::create_dir_all(lock_path(&dir).parent().unwrap()).unwrap();
    fs::write(
        lock_path(&dir),
        toml::to_string(&AppLockContents {
            pid: dead_pid,
            hostname: HOST.to_string(),
            started_at: Utc::now(),
            project_root: project_root(&dir),
        })
        .unwrap(),
    )
    .unwrap();
    let guard = acquire_default(&dir, &MockProbe::dead(), Utc::now()).expect("acquire");
    let parsed: AppLockContents =
        toml::from_str(&fs::read_to_string(guard.path()).unwrap()).unwrap();
    assert_eq!(
        parsed.pid, SELF_PID,
        "stale lock must be replaced with ours"
    );
}

#[test]
fn replaces_stale_lock_when_pid_recycled_with_mismatched_start_time() {
    let dir = TempDir::new().unwrap();
    let pid = 9001;
    let recorded = Utc::now() - chrono::Duration::hours(2);
    fs::create_dir_all(lock_path(&dir).parent().unwrap()).unwrap();
    fs::write(
        lock_path(&dir),
        toml::to_string(&AppLockContents {
            pid,
            hostname: HOST.to_string(),
            started_at: recorded,
            project_root: project_root(&dir),
        })
        .unwrap(),
    )
    .unwrap();
    // The pid is alive, but reports a start time well outside tolerance and
    // an executable that isn't codexize → recycled, replace.
    let probe = MockProbe::live(pid, Utc::now(), PathBuf::from("/bin/sleep"));
    let guard = acquire_default(&dir, &probe, Utc::now()).expect("acquire");
    let parsed: AppLockContents =
        toml::from_str(&fs::read_to_string(guard.path()).unwrap()).unwrap();
    assert_eq!(parsed.pid, SELF_PID);
}

#[test]
fn refuses_cross_host_lock_with_pinned_message_and_no_mutation() {
    let dir = TempDir::new().unwrap();
    let other_host = "other-host";
    let other_pid = 5678;
    fs::create_dir_all(lock_path(&dir).parent().unwrap()).unwrap();
    let original = toml::to_string(&AppLockContents {
        pid: other_pid,
        hostname: other_host.to_string(),
        started_at: Utc::now(),
        project_root: project_root(&dir),
    })
    .unwrap();
    fs::write(lock_path(&dir), &original).unwrap();
    // Even if the local probe would say "alive" for the recorded pid, we
    // must NOT probe (or mutate) — the cross-host branch is decided purely
    // from the on-disk hostname.
    let probe = MockProbe::live(other_pid, Utc::now(), PathBuf::from("/usr/bin/codexize"));
    let err = acquire_default(&dir, &probe, Utc::now()).expect_err("must refuse");
    match err {
        AcquireError::OwnedByOtherHost { hostname, pid } => {
            assert_eq!(hostname, other_host);
            assert_eq!(pid, other_pid);
        }
        other => panic!("expected OwnedByOtherHost, got {other:?}"),
    }
    let after = fs::read_to_string(lock_path(&dir)).unwrap();
    assert_eq!(after, original, "cross-host lock must be untouched");
    let msg = AcquireError::OwnedByOtherHost {
        hostname: other_host.to_string(),
        pid: other_pid,
    }
    .to_string();
    assert!(
        msg.contains("codexize lock owned by host other-host (pid 5678)"),
        "missing host/pid in message: {msg}"
    );
    assert!(
        msg.contains("remove .codexize/app.lock manually"),
        "missing manual-remove guidance in message: {msg}"
    );
}

#[test]
fn release_returns_ok_when_lock_already_gone() {
    let dir = TempDir::new().unwrap();
    let guard = acquire_default(&dir, &MockProbe::dead(), Utc::now()).expect("acquire");
    fs::remove_file(guard.path()).unwrap();
    guard
        .release()
        .expect("release on missing file must succeed");
}

#[test]
fn release_refuses_to_remove_lock_owned_by_another_pid() {
    // Simulates the race where stale-break replaced our lock with a peer's.
    let dir = TempDir::new().unwrap();
    let guard = acquire_default(&dir, &MockProbe::dead(), Utc::now()).expect("acquire");
    let path = guard.path().to_path_buf();
    fs::write(
        &path,
        toml::to_string(&AppLockContents {
            pid: SELF_PID + 1,
            hostname: HOST.to_string(),
            started_at: Utc::now(),
            project_root: project_root(&dir),
        })
        .unwrap(),
    )
    .unwrap();
    guard.release().expect("foreign-owned lock left intact");
    assert!(path.exists(), "must not remove another process's lock");
}

#[test]
fn malformed_existing_lock_is_replaced() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(lock_path(&dir).parent().unwrap()).unwrap();
    fs::write(lock_path(&dir), "not valid = toml = at all").unwrap();
    let guard = acquire_default(&dir, &MockProbe::dead(), Utc::now()).expect("acquire");
    let parsed: AppLockContents =
        toml::from_str(&fs::read_to_string(guard.path()).unwrap()).unwrap();
    assert_eq!(parsed.pid, SELF_PID);
}

#[test]
fn release_failure_propagates_through_explicit_release() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let guard = acquire_default(&dir, &MockProbe::dead(), Utc::now()).expect("acquire");
    let parent = guard.path().parent().unwrap().to_path_buf();
    let original = fs::metadata(&parent).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_mode(0o555);
    fs::set_permissions(&parent, readonly).unwrap();
    let result = guard.release();
    fs::set_permissions(&parent, original).unwrap();
    let err = result.expect_err("release must surface remove_file errors");
    assert!(
        format!("{err:#}").contains("failed to remove lock"),
        "missing context in error"
    );
}
