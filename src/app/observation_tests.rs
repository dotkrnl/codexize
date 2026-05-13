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
        subscription_label: "moonshotai".to_string(),
        window_name: "[Test]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Tough,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
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

