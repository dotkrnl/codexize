use codexize::logic::pipeline::{Phase, RunRecord, RunStatus};
use codexize::logic::rules::{RetryTarget, retry_phase_for_stage, retry_target_for_run};

#[test]
fn retry_phase_for_stage_keeps_stage_to_phase_mapping_pure() {
    assert_eq!(
        retry_phase_for_stage("plan-review"),
        Some(Phase::PlanReviewRunning)
    );
    assert_eq!(retry_phase_for_stage("unknown"), None);
}

#[test]
fn retry_target_for_run_prefers_task_and_falls_back_to_stage() {
    let task_run = RunRecord {
        id: 7,
        stage: "coder".to_string(),
        task_id: Some(42),
        round: 2,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "codex".to_string(),
        window_name: "coder".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: Default::default(),
        modes: Default::default(),
        hostname: None,
        mount_device_id: None,
    };
    assert_eq!(retry_target_for_run(&task_run), Some(RetryTarget::Task(42)));

    let stage_run = RunRecord {
        task_id: None,
        stage: "planning".to_string(),
        ..task_run
    };
    assert_eq!(
        retry_target_for_run(&stage_run),
        Some(RetryTarget::Stage("planning"))
    );
}
