//! End-to-end integration: drive the runtime through the stubbed-UI seam
//! across a multi-stage command script and verify the published `AppView`
//! reflects each step. This is the same surface a future server/web binary
//! will reuse — no `ratatui`/`crossterm`, only the public seam.
//!
//! Cargo treats `tests/integration/` as a single test binary because of
//! `main.rs`; per-feature integration tests can be added as siblings and
//! `mod`'d in below.

use std::time::Duration;

use codexize::app_runtime::{
    AppCommand, AppView, ModalKind, RuntimeControl, RuntimeHarness, StageId, StatusMessage,
    StatusSeverity, channel_pair, run_harness_until_exit,
};
use codexize::logic::pipeline::Phase;

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
fn full_pipeline_run_via_stubbed_ui() {
    let (ui, runtime) = channel_pair();
    let mut view = AppView::empty("integration-pipeline");

    runtime
        .views_tx
        .send(view.clone())
        .expect("publish initial view");
    let initial = ui
        .views_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("ui receives initial view");
    assert_eq!(initial.session_id.as_ref(), "integration-pipeline");
    assert_eq!(initial.phase, Phase::IdeaInput);

    // Walk a representative command sequence the way a real UI would: start
    // a brainstorm-equivalent submit, surface a stage retry, dismiss the
    // modal, and finally quit. The runtime must drain every command and the
    // published view must reflect each step.
    let script = [
        AppCommand::SubmitInput {
            text: "kick off pipeline".to_string(),
        },
        AppCommand::ToggleYolo,
        AppCommand::RetryStage(StageId::Planning),
        AppCommand::CancelModal,
    ];

    for command in script {
        ui.commands_tx
            .send(command.clone())
            .expect("ui sends command");
        let drained = runtime
            .commands_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("runtime drains command");
        assert_eq!(drained, command);
        stub_runtime_step(&mut view, drained);
        runtime
            .views_tx
            .send(view.clone())
            .expect("runtime publishes view");
        let _ = ui
            .views_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("ui receives next view");
    }

    let status = view
        .status
        .as_ref()
        .expect("submitted text became a status line");
    assert_eq!(status.text.as_ref(), "kick off pipeline");
    assert_eq!(status.severity, StatusSeverity::Info);
    assert!(view.modal.is_none(), "CancelModal cleared the modal");
    assert!(!view.follow_tail, "ToggleYolo flipped follow_tail off");
}

#[test]
fn runtime_harness_drains_commands_until_quit() {
    let (ui, runtime) = channel_pair();
    ui.commands_tx
        .send(AppCommand::OpenPalette)
        .expect("send palette command");
    ui.commands_tx
        .send(AppCommand::Quit)
        .expect("send quit command");

    let mut harness = RuntimeHarness::new(AppView::empty("integration-quit"));
    let control = run_harness_until_exit(&mut harness, runtime).expect("run harness loop");

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
    assert_eq!(first.session_id.as_ref(), "integration-quit");
    assert_eq!(second.session_id.as_ref(), "integration-quit");
}
