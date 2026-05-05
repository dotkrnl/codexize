//! Public-surface harness test: drive the runtime/UI seam end-to-end
//! without any terminal IO.
//!
//! The TUI extraction is staged, but the seam types it will publish on
//! ([`AppView`], [`AppCommand`]) and the channel pair the future server
//! mode will reuse must be reachable through the crate's public surface
//! and exercise without `ratatui`/`crossterm`. This test pins those
//! contracts so subsequent slices cannot quietly break the stubbed-UI path.

use std::time::Duration;

use codexize::app_runtime::{
    AgentRunSummary, AppCommand, AppView, ModalKind, RuntimeControl, RuntimeHarness, StageId,
    StatusSeverity, channel_pair, headless_runtime_for_live_summary, run_harness_until_exit,
    run_headless_until_exit,
};
use codexize::logic::pipeline::Phase;
use tempfile::tempdir;

#[test]
fn channels_carry_a_full_command_view_round_trip() {
    let dir = tempdir().expect("tempdir");
    let live_summary_path = dir.path().join("live.txt");
    std::fs::write(&live_summary_path, "approved").expect("seed");
    let (ui, runtime) = channel_pair();

    let script = [
        AppCommand::ToggleYolo,
        AppCommand::RetryStage(StageId::Sharding),
        AppCommand::CancelModal,
        AppCommand::SubmitInput {
            text: "approved".to_string(),
        },
    ];

    for command in script {
        ui.commands_tx.send(command).expect("ui sends command");
    }
    let mut app_runtime =
        headless_runtime_for_live_summary("integration-session", &live_summary_path);
    let control = run_headless_until_exit(&mut app_runtime, runtime).expect("run headless runtime");

    assert_eq!(control, RuntimeControl::Continue);
    let snapshots: Vec<_> = ui.views_rx.try_iter().collect();
    assert_eq!(snapshots[0].session_id.as_ref(), "integration-session");
    assert_eq!(snapshots[0].phase, Phase::IdeaInput);
    assert!(snapshots[0].agent_runs.is_empty());
    assert!(snapshots[1].modes.yolo, "ToggleYolo flipped yolo on");
    assert_eq!(
        snapshots[2].modal,
        Some(ModalKind::StageError(StageId::Sharding))
    );
    assert!(
        snapshots[3].modal.is_none(),
        "modal should clear after CancelModal"
    );
    let status = snapshots[4]
        .status
        .as_ref()
        .expect("submitted text triggered data status");
    assert_eq!(status.text.as_ref(), "approved");
    assert_eq!(status.severity, StatusSeverity::Info);
}

#[test]
fn agent_run_summary_is_constructible_from_public_surface() {
    use chrono::Utc;
    use codexize::adapters::EffortLevel;
    use codexize::logic::pipeline::{LaunchModes, RunRecord, RunStatus};

    let run = RunRecord {
        id: 7,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt: 0,
        model: "gpt-test".to_string(),
        vendor: "test-vendor".to_string(),
        window_name: "codexize-run-7-planning".to_string(),
        started_at: Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    };
    let summary = AgentRunSummary::from_record(&run);
    assert_eq!(summary.id, 7);
    assert_eq!(summary.stage.as_ref(), "planning");
    assert_eq!(summary.window_name.as_ref(), "codexize-run-7-planning");
    assert_eq!(summary.status, RunStatus::Running);
}

#[test]
fn runtime_harness_drains_commands_and_publishes_views_until_exit() {
    let (ui, runtime) = channel_pair();
    ui.commands_tx
        .send(AppCommand::OpenPalette)
        .expect("send palette command");
    ui.commands_tx
        .send(AppCommand::Quit)
        .expect("send quit command");

    let mut harness = RuntimeHarness::new(AppView::empty("runtime-loop"));
    let control = run_harness_until_exit(&mut harness, runtime).expect("run harness");

    assert_eq!(control, RuntimeControl::Exit);
    assert_eq!(
        harness.commands(),
        &[AppCommand::OpenPalette, AppCommand::Quit]
    );

    let first = ui
        .views_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first view");
    let second = ui
        .views_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("second view");
    assert_eq!(first.session_id.as_ref(), "runtime-loop");
    assert_eq!(second.session_id.as_ref(), "runtime-loop");
}
