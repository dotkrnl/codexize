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
fn new_creates_missing_cache_dir() {
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("cache");
    assert!(!dir.exists());
    let _ = CacheWatcher::start(&dir, None);
    assert!(dir.is_dir(), "watcher must mkdir -p the cache directory");
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
fn poll_fires_once_after_publish() {
    let dir = TempDir::new().unwrap();
    let initial = write_models(dir.path(), "{}");
    let mut watcher = match CacheWatcher::start(dir.path(), Some(initial)) {
        CacheWatcherOutcome::Active(w) | CacheWatcherOutcome::PollOnly { watcher: w, .. } => w,
    };
    // Force the poll branch on the next call so the test does not depend
    // on the notify backend firing within a deterministic window. The
    // poll cadence is 60 s in production; we cheat by reaching into the
    // poll deadline and setting it to "already due".
    watcher.next_poll_after = std::time::Instant::now();
    // Same mtime as initial — no reload.
    assert!(
        !watcher.poll(),
        "poll with unchanged mtime must not signal a reload"
    );
    // New publish (advance the mtime by writing fresh bytes after a
    // short sleep; some file systems only carry second-resolution mtime).
    std::thread::sleep(Duration::from_millis(1100));
    let _ = write_models(dir.path(), "{\"version\":10}");
    watcher.next_poll_after = std::time::Instant::now();
    assert!(
        watcher.poll(),
        "poll after publish must signal a reload exactly once"
    );
    // Subsequent polls without further publishes must NOT re-fire — that
    // is the debounce contract the spec calls for.
    watcher.next_poll_after = std::time::Instant::now();
    assert!(
        !watcher.poll(),
        "debounce: second poll with same mtime must not signal a reload"
    );
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
