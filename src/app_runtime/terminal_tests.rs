use super::*;
use crate::app_runtime::{AgentRunSummary, AppCommand, AppView, ModalKind};
use crate::data::events::{DataEvent, DataOutcome, DataRequest, LiveSummaryEvents};
use crate::state::RunStatus;
use std::sync::Arc;
use tokio::sync::mpsc;

fn running_view() -> AppView {
    let mut view = AppView::empty("terminal-runtime-test");
    view.agent_running = true;
    view.agent_runs = Arc::from(vec![AgentRunSummary {
        id: 7,
        stage: Arc::from("planning"),
        window_name: Arc::from("codexize-run-7-planning"),
        status: RunStatus::Running,
    }]);
    view
}

#[test]
fn confirm_quit_running_agent_routes_termination_through_data_without_app_mutation() {
    let mut runtime = TerminalRuntime::default();
    let view = running_view();

    let quit = runtime.route_command_with_dispatch(AppCommand::Quit, &view, |_| {
        panic!("opening the runtime modal must not perform data side effects")
    });
    assert_eq!(quit, TerminalCommandOutcome::HandledContinue);
    assert_eq!(
        runtime.view_for_render(view.clone()).modal,
        Some(ModalKind::QuitRunningAgent)
    );

    let mut requests = Vec::new();
    let confirm = runtime.route_command_with_dispatch(AppCommand::ConfirmModal, &view, |request| {
        requests.push(request);
        DataOutcome::Terminated(true)
    });

    assert_eq!(confirm, TerminalCommandOutcome::HandledExit);
    assert_eq!(requests, vec![DataRequest::TerminateRun { run_id: 7 }]);
}

#[test]
fn confirm_app_owned_quit_modal_routes_termination_through_runtime() {
    // Regression: production `:quit` opens the App-owned modal (sets only
    // `view.modal`), so a subsequent Enter must still route termination
    // through the runtime even when `modal_override` is None. Drives the
    let mut runtime = TerminalRuntime::default();
    let mut view = running_view();
    view.modal = Some(ModalKind::QuitRunningAgent);

    let mut requests = Vec::new();
    let confirm = runtime.route_command_with_dispatch(AppCommand::ConfirmModal, &view, |request| {
        requests.push(request);
        DataOutcome::Terminated(true)
    });

    assert_eq!(confirm, TerminalCommandOutcome::HandledExit);
    assert_eq!(requests, vec![DataRequest::TerminateRun { run_id: 7 }]);
}

#[test]
fn runtime_drains_live_summary_watcher_as_data_events() {
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(()).expect("send watcher signal");
    tx.send(()).expect("send watcher signal");
    let mut events = LiveSummaryEvents::new(rx);

    assert_eq!(
        TerminalRuntime::default().drain_live_summary_data_events(Some(&mut events)),
        vec![DataEvent::LiveSummaryChanged, DataEvent::LiveSummaryChanged]
    );
    assert_eq!(
        TerminalRuntime::default().drain_live_summary_data_events(Some(&mut events)),
        vec![]
    );
}
