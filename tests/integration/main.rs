//! End-to-end integration: drive the real app-runtime headless entrypoint
//! through the stubbed-UI seam and verify the published `AppView` snapshots
//! reflect logic decisions plus data-dispatch outcomes. This is the same
//! surface a future server/web binary will reuse — no `ratatui`/`crossterm`,
//! only the public seam.
//!
//! Cargo treats `tests/integration/` as a single test binary because of
//! `main.rs`; per-feature integration tests can be added as siblings and
//! `mod`'d in below.

mod app_runtime_harness;
mod config;
mod layer_boundaries;
mod smoke_baseline;
mod support;

use codexize::app_runtime::{AppCommand, AppView, ModalKind, StageId};
use codexize::logic::pipeline::Phase;
use support::{
    RuntimeControl, RuntimeHarness, channel_pair, drain_views, headless_runtime_for_live_summary,
    run_harness_until_exit, run_headless_until_exit,
};
use tempfile::tempdir;

#[test]
fn full_pipeline_run_via_stubbed_ui() {
    // Real-data fixture: a temp file the runtime will read through
    // `data::events::dispatch` when the UI submits an input.
    let dir = tempdir().expect("tempdir");
    let live_summary_path = dir.path().join("live_summary.txt");
    std::fs::write(&live_summary_path, "pipeline-progress-line")
        .expect("seed live-summary fixture");

    let (mut ui, runtime) = channel_pair();

    // The UI script: walk a representative command sequence the way a real
    // UI would: surface a stage retry (exercises a real logic decision),
    // submit an input that pulls real data through the dispatch surface,
    // toggle YOLO, dismiss the modal, then quit.
    ui.commands_tx
        .send(AppCommand::RetryStage(StageId::Brainstorm))
        .expect("ui sends RetryStage");
    ui.commands_tx
        .send(AppCommand::SubmitInput {
            text: "fallback-status".to_string(),
        })
        .expect("ui sends SubmitInput");
    ui.commands_tx
        .send(AppCommand::ToggleYolo)
        .expect("ui sends ToggleYolo");
    ui.commands_tx
        .send(AppCommand::CancelModal)
        .expect("ui sends CancelModal");
    ui.commands_tx
        .send(AppCommand::Quit)
        .expect("ui sends Quit");

    let mut app_runtime =
        headless_runtime_for_live_summary("integration-pipeline", &live_summary_path);
    assert_eq!(app_runtime.view().phase, Phase::IdeaInput);

    let control = run_headless_until_exit(&mut app_runtime, runtime).expect("headless runtime");
    assert_eq!(control, RuntimeControl::Exit);

    let snapshots: Vec<_> = drain_views(&mut ui.views_rx);
    assert_eq!(
        snapshots.len(),
        6,
        "initial view plus one runtime-published snapshot per command"
    );
    assert_eq!(snapshots[0].phase, Phase::IdeaInput);
    assert_eq!(
        snapshots[1].phase,
        Phase::BrainstormRunning,
        "retry_phase_for_stage(\"brainstorm\") drives the published phase"
    );
    assert_eq!(
        snapshots[1].modal,
        Some(ModalKind::StageError(StageId::Brainstorm))
    );

    let status = snapshots[2]
        .status
        .as_ref()
        .expect("SubmitInput populated the status line via data dispatch");
    assert_eq!(
        status.text.as_ref(),
        "pipeline-progress-line",
        "data::events::dispatch returned the real file content"
    );
    assert_eq!(status.severity, codexize::app_runtime::StatusSeverity::Info);
    assert!(
        snapshots[3].modes.yolo,
        "ToggleYolo flipped the published modes"
    );
    assert!(
        snapshots[4].modal.is_none(),
        "CancelModal cleared the modal"
    );
    assert_eq!(snapshots[5].modal, Some(ModalKind::QuitRunningAgent));
}

#[test]
fn submit_input_falls_back_when_live_summary_missing() {
    // Drives the same real `data::events::dispatch` path but against a
    // missing path, so `LiveSummaryRead(None)` is the runtime-observable
    // outcome. Proves the runtime surfaces typed `DataOutcome` differences,
    // not a single hardcoded string.
    let dir = tempdir().expect("tempdir");
    let absent_path = dir.path().join("never-written.txt");

    let (mut ui, runtime) = channel_pair();
    ui.commands_tx
        .send(AppCommand::SubmitInput {
            text: "no-data-yet".to_string(),
        })
        .expect("ui sends SubmitInput");

    let mut app_runtime = headless_runtime_for_live_summary("integration-missing", &absent_path);
    let control = run_headless_until_exit(&mut app_runtime, runtime).expect("headless runtime");
    assert_eq!(control, RuntimeControl::Continue);

    let snapshots: Vec<_> = drain_views(&mut ui.views_rx);
    assert_eq!(snapshots.len(), 2);
    let status = snapshots[1]
        .status
        .as_ref()
        .expect("missing live-summary still produces a status fallback");
    assert_eq!(status.text.as_ref(), "no-data-yet");
    assert_eq!(status.severity, codexize::app_runtime::StatusSeverity::Warn);
}

#[test]
fn runtime_harness_drains_commands_until_quit() {
    let (mut ui, runtime) = channel_pair();
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

    let first = ui.views_rx.blocking_recv().expect("first view");
    let second = ui.views_rx.blocking_recv().expect("second view");
    assert_eq!(first.session_id.as_ref(), "integration-quit");
    assert_eq!(second.session_id.as_ref(), "integration-quit");
}
