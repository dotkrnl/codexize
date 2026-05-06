use super::*;
use crate::adapters::EffortLevel;
use crate::state::LaunchModes;
use chrono::Utc;

fn sample_run(id: u64, stage: &str, status: RunStatus) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round: 0,
        attempt: 0,
        model: "test-model".to_string(),
        vendor: "test-vendor".to_string(),
        window_name: format!("codexize-run-{id}-{stage}"),
        started_at: Utc::now(),
        ended_at: None,
        status,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

#[test]
fn empty_view_has_no_modal_or_status() {
    let view = AppView::empty("test-session");
    assert_eq!(view.session_id.as_ref(), "test-session");
    assert!(view.modal.is_none());
    assert!(view.status.is_none());
    assert!(view.agent_runs.is_empty());
    assert!(view.follow_tail);
    assert!(!view.agent_running);
    assert_eq!(view.phase, Phase::IdeaInput);
    assert_eq!(view.modes, ModeFlags::default());
}

#[test]
fn agent_run_summary_projects_record_fields() {
    let run = sample_run(42, "brainstorm", RunStatus::Running);
    let summary = AgentRunSummary::from_record(&run);
    assert_eq!(summary.id, 42);
    assert_eq!(summary.stage.as_ref(), "brainstorm");
    assert_eq!(summary.window_name.as_ref(), "codexize-run-42-brainstorm");
    assert_eq!(summary.status, RunStatus::Running);
}
