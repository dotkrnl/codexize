//! Public-surface harness test: drive the runtime/UI seam end-to-end
//! without any terminal IO.
//!
//! The TUI extraction is staged: today the production runtime still owns
//! state through `App`, but the seam types it will publish on
//! ([`AppView`], [`AppCommand`]) and the channel pair the future server
//! mode will reuse must be reachable through the crate's public surface
//! and exercise without `ratatui`/`crossterm`. This test pins both
//! contracts so that subsequent slices of the refactor cannot quietly
//! break the stubbed-UI path.

use std::time::Duration;

use codexize::app_runtime::{
    AgentRunSummary, AppCommand, AppView, ModalKind, RuntimeControl, RuntimeHarness, StageId,
    StatusMessage, StatusSeverity, channel_pair, run_harness_until_exit,
};
use codexize::logic::pipeline::Phase;

/// Mirrors the stub used in the in-tree unit test, kept here so the
/// integration crate proves the seam from outside the library.
fn stub_runtime_step(view: &mut AppView, command: AppCommand) {
    match command {
        AppCommand::Quit => view.modal = Some(ModalKind::QuitRunningAgent),
        AppCommand::OpenPalette => view.modal = None,
        AppCommand::ToggleYolo => view.follow_tail = !view.follow_tail,
        AppCommand::RetryStage(stage) => view.modal = Some(ModalKind::StageError(stage)),
        AppCommand::CancelModal => view.modal = None,
        AppCommand::SubmitInput { text } => {
            view.status = Some(StatusMessage {
                text: text.into(),
                severity: StatusSeverity::Info,
            });
        }
        _ => {}
    }
}

#[test]
fn channels_carry_a_full_command_view_round_trip() {
    let (ui, runtime) = channel_pair();
    let mut view = AppView::empty("integration-session");

    runtime
        .views_tx
        .send(view.clone())
        .expect("publish initial view");
    let initial = ui
        .views_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("ui receives initial view");
    assert_eq!(initial.session_id.as_ref(), "integration-session");
    assert_eq!(initial.phase, Phase::IdeaInput);
    assert!(initial.agent_runs.is_empty());

    let script = [
        AppCommand::ToggleYolo,
        AppCommand::RetryStage(StageId::Sharding),
        AppCommand::CancelModal,
        AppCommand::SubmitInput {
            text: "approved".to_string(),
        },
    ];

    for command in script {
        ui.commands_tx
            .send(command.clone())
            .expect("ui sends command");
        let received = runtime
            .commands_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("runtime drains command");
        assert_eq!(received, command);
        stub_runtime_step(&mut view, received);
        runtime
            .views_tx
            .send(view.clone())
            .expect("runtime publishes view");
        let _next = ui
            .views_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("ui receives next view");
    }

    assert!(view.modal.is_none(), "modal should clear after CancelModal");
    let status = view.status.as_ref().expect("submitted text became status");
    assert_eq!(status.text.as_ref(), "approved");
    assert_eq!(status.severity, StatusSeverity::Info);
    assert!(!view.follow_tail, "ToggleYolo flipped follow_tail off");
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
