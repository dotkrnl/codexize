use super::live_summary_advances_content;
use crate::adapters::EffortLevel;
use crate::app::test_support::mk_app;
use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};
use std::time::Duration;
use tempfile::tempdir;

fn running_run(id: u64) -> RunRecord {
    RunRecord {
        id,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "kimi-k2.6".to_string(),
        vendor: "moonshotai".to_string(),
        route_provider: None,
        window_name: "[Test]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Tough,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

#[test]
fn empty_sanitized_payload_is_not_a_content_advance() {
    assert!(!live_summary_advances_content("", ""));
    assert!(!live_summary_advances_content("", "prior"));
}

#[test]
fn duplicate_sanitized_payload_is_not_a_content_advance() {
    assert!(!live_summary_advances_content("same", "same"));
}

#[test]
fn fresh_sanitized_payload_is_a_content_advance() {
    assert!(live_summary_advances_content("first", ""));
    assert!(live_summary_advances_content("second", "first"));
}

#[tokio::test(start_paused = true)]
async fn duplicate_live_summary_write_does_not_reset_watchdog_idle_clock() {
    // Operator-visible behavior: an agent that re-flushes identical content
    // (same title|summary) must not reset the watchdog clock — the warning
    // must still fire 15 minutes after the LAST real content change, not
    // 15 minutes after the most recent mtime bump.
    let dir = tempdir().expect("tempdir");
    let live_path = dir.path().join("live_summary.txt");

    let mut state = SessionState::new("watchdog-test".to_string());
    state.agent_runs.push(running_run(1));
    let mut app = mk_app(state);
    app.current_run_id = Some(1);
    app.live_summary_path = Some(live_path.clone());

    let now = tokio::time::Instant::now();
    app.watchdog.register(
        1,
        EffortLevel::Tough,
        "[Test]".to_string(),
        live_path.clone(),
        now,
    );

    std::fs::write(&live_path, "title|first content").expect("write 1");
    app.read_live_summary_pipeline();
    let elapsed_after_first = app
        .watchdog
        .get(1)
        .expect("watchdog state")
        .idle_elapsed(tokio::time::Instant::now());
    assert!(
        elapsed_after_first < Duration::from_secs(1),
        "first real content write should reset the idle clock to ~zero, got {:?}",
        elapsed_after_first
    );

    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    // Real-time sleep so the filesystem mtime advances between writes —
    // tokio's paused clock doesn't move filetime.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(&live_path, "title|first content").expect("write 2 (identical)");
    app.read_live_summary_pipeline();
    let elapsed_after_dup = app
        .watchdog
        .get(1)
        .expect("watchdog state")
        .idle_elapsed(tokio::time::Instant::now());
    assert!(
        elapsed_after_dup >= Duration::from_secs(5 * 60),
        "duplicate-content write must NOT reset the watchdog clock; got elapsed {:?}",
        elapsed_after_dup
    );
}
