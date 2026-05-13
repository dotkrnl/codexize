use super::*;
use std::io::Write;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn write_models(dir: &std::path::Path, body: &str) -> SystemTime {
    let path = dir.join("models.json");
    let mut f = std::fs::File::create(&path).expect("create models.json");
    f.write_all(body.as_bytes()).expect("write models.json");
    f.sync_all().expect("sync models.json");
    std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .expect("read mtime")
}

#[test]
fn poll_returns_false_when_cache_file_missing() {
    let dir = TempDir::new().unwrap();
    let mut watcher = match CacheWatcher::start(dir.path(), None) {
        CacheWatcherOutcome::Active(w) | CacheWatcherOutcome::PollOnly { watcher: w, .. } => w,
    };
    assert!(!watcher.poll(), "no file, no reload");
}

#[test]
fn poll_fires_once_when_no_baseline() {
    let dir = TempDir::new().unwrap();
    let mut watcher = match CacheWatcher::start(dir.path(), None) {
        CacheWatcherOutcome::Active(w) | CacheWatcherOutcome::PollOnly { watcher: w, .. } => w,
    };
    let _ = write_models(dir.path(), "{}");
    watcher.next_poll_after = std::time::Instant::now();
    assert!(
        watcher.poll(),
        "first publish from a None baseline must trigger a reload"
    );
    watcher.next_poll_after = std::time::Instant::now();
    assert!(!watcher.poll(), "no further change → no further reload");
}
