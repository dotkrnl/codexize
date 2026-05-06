use super::*;
use crate::logic::pipeline::Phase;
use tempfile::tempdir;

/// Minimal stubbed runtime that reacts to commands by mutating an
/// [`AppView`] and publishing the next snapshot. This is not the
/// production runtime — it is a shape proof that the seam works.
fn stub_runtime_step(view: &mut AppView, command: AppCommand) {
    match command {
        AppCommand::Quit => {
            view.modal = Some(ModalKind::QuitRunningAgent);
        }
        AppCommand::OpenPalette => {
            view.modal = None;
        }
        AppCommand::ToggleYolo => {
            view.follow_tail = !view.follow_tail;
        }
        AppCommand::RetryStage(stage) => {
            view.modal = Some(ModalKind::StageError(stage));
        }
        AppCommand::CancelModal => {
            view.modal = None;
        }
        _ => {}
    }
}

#[test]
fn channel_pair_exchanges_commands_and_views_without_terminal() {
    let (mut ui, mut runtime) = channel_pair();

    // Seed an initial view as the runtime would.
    let mut view = AppView::empty("session-stub");
    runtime
        .views_tx
        .send(view.clone())
        .expect("publish initial view");

    let initial = ui
        .views_rx
        .blocking_recv()
        .expect("ui receives initial view");
    assert_eq!(initial.phase, Phase::IdeaInput);
    assert!(initial.modal.is_none());

    // UI emits a command; runtime drains and republishes.
    ui.commands_tx
        .send(AppCommand::Quit)
        .expect("ui sends quit");
    let received = runtime
        .commands_rx
        .blocking_recv()
        .expect("runtime drains command");
    assert_eq!(received, AppCommand::Quit);
    stub_runtime_step(&mut view, received);
    runtime
        .views_tx
        .send(view.clone())
        .expect("publish updated view");

    let after_quit = ui
        .views_rx
        .blocking_recv()
        .expect("ui receives updated view");
    assert_eq!(after_quit.modal, Some(ModalKind::QuitRunningAgent));
}

#[test]
fn runtime_can_drive_a_full_round_trip_without_ui_state() {
    let (ui, mut runtime) = channel_pair();
    let mut views_rx = ui.views_rx;
    let mut view = AppView::empty("session-stub");

    let script = [
        AppCommand::OpenPalette,
        AppCommand::RetryStage(StageId::Implementation),
        AppCommand::CancelModal,
    ];

    for command in script {
        ui.commands_tx
            .send(command.clone())
            .expect("ui sends command");
        let received = runtime.commands_rx.blocking_recv().expect("runtime drains");
        stub_runtime_step(&mut view, received);
        runtime.views_tx.send(view.clone()).expect("publish view");
        let _ = views_rx.blocking_recv().expect("ui receives view");
    }

    // After the script the modal returns to None — proves command
    // routing is bidirectional and pure-value.
    assert!(view.modal.is_none());
}

#[test]
fn headless_runtime_routes_logic_and_data_before_publishing() {
    let dir = tempdir().expect("tempdir");
    let live_summary_path = dir.path().join("live.txt");
    std::fs::write(&live_summary_path, "runtime-owned").expect("seed");
    let (mut ui, channels) = channel_pair();
    ui.commands_tx
        .send(AppCommand::RetryStage(StageId::Brainstorm))
        .expect("send retry");
    ui.commands_tx
        .send(AppCommand::SubmitInput {
            text: "fallback".to_string(),
        })
        .expect("send input");
    ui.commands_tx.send(AppCommand::Quit).expect("send quit");

    let mut runtime = headless_runtime_for_live_summary("headless", &live_summary_path);
    let control = run_headless_until_exit(&mut runtime, channels).expect("run");

    assert_eq!(control, RuntimeControl::Exit);
    assert_eq!(runtime.view().phase, Phase::BrainstormRunning);
    let snapshots: Vec<_> = drain_views(&mut ui.views_rx);
    assert_eq!(snapshots.len(), 4);
    assert_eq!(snapshots[1].phase, Phase::BrainstormRunning);
    assert_eq!(
        snapshots[2].status.as_ref().unwrap().text.as_ref(),
        "runtime-owned"
    );
    assert_eq!(snapshots[3].modal, Some(ModalKind::QuitRunningAgent));
}

fn drain_views(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppView>) -> Vec<AppView> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}
