//! Production terminal runtime coordinator.
//!
//! The TUI owns crossterm event collection and terminal drawing, while this
//! module owns the application loop ordering: pre-drain tick, post-drain
//! tick, render, then command dispatch.

use anyhow::Result;

use crate::app_runtime::{AppCommand, AppView, ModalKind};
use crate::data::events::{DataEvent, DataOutcome, DataRequest, LiveSummaryEvents};
use crate::logic::pipeline::RunStatus;
use crate::{app::App, tui::AppTerminal};

/// Result of routing an [`AppCommand`] through the terminal runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalCommandOutcome {
    /// Runtime handled the command and should keep running.
    HandledContinue,
    /// Runtime handled the command and should exit the app loop.
    HandledExit,
    /// Command is still owned by the legacy App bridge.
    Legacy(AppCommand),
}

/// Runtime-owned production state that is not part of the legacy `App`.
///
/// This keeps the migrated quit-confirmation path outside `App`: the UI emits
/// commands, app_runtime owns modal state and side-effect dispatch, and App is
/// only asked to handle commands that have not yet moved across the seam.
#[derive(Default)]
pub(crate) struct TerminalRuntime {
    modal_override: Option<ModalKind>,
}

impl TerminalRuntime {
    pub(crate) fn view_for_render(&self, mut view: AppView) -> AppView {
        if let Some(modal) = self.modal_override {
            view.modal = Some(modal);
        }
        view
    }

    pub(crate) fn drain_live_summary_data_events(
        &self,
        events: Option<&LiveSummaryEvents>,
    ) -> Vec<DataEvent> {
        events.map(LiveSummaryEvents::drain).unwrap_or_default()
    }

    fn drain_app_data_events(&mut self, app: &mut App) {
        let drained = self.drain_live_summary_data_events(app.live_summary_change_events.as_ref());
        if drained
            .iter()
            .any(|event| matches!(event, DataEvent::LiveSummaryChanged))
        {
            app.read_live_summary_pipeline();
        }
        app.poll_live_summary_fallback();
    }

    pub(crate) fn route_command(
        &mut self,
        command: AppCommand,
        view: &AppView,
    ) -> TerminalCommandOutcome {
        self.route_command_with_dispatch(command, view, crate::data::events::dispatch)
    }

    pub(crate) fn route_command_with_dispatch<F>(
        &mut self,
        command: AppCommand,
        view: &AppView,
        mut dispatch: F,
    ) -> TerminalCommandOutcome
    where
        F: FnMut(DataRequest) -> DataOutcome,
    {
        match command {
            AppCommand::Quit if view.agent_running => {
                self.modal_override = Some(ModalKind::QuitRunningAgent);
                TerminalCommandOutcome::HandledContinue
            }
            AppCommand::ConfirmModal
                if matches!(self.modal_override, Some(ModalKind::QuitRunningAgent)) =>
            {
                for run in view
                    .agent_runs
                    .iter()
                    .filter(|run| run.status == RunStatus::Running)
                {
                    let _ = dispatch(DataRequest::TerminateRun {
                        window_name: run.window_name.to_string(),
                    });
                }
                TerminalCommandOutcome::HandledExit
            }
            AppCommand::CancelModal
                if matches!(self.modal_override, Some(ModalKind::QuitRunningAgent)) =>
            {
                self.modal_override = None;
                TerminalCommandOutcome::HandledContinue
            }
            other => TerminalCommandOutcome::Legacy(other),
        }
    }
}

/// Run the production terminal app through the app-runtime seam.
pub fn run_terminal_app(app: &mut App, terminal: &mut AppTerminal) -> Result<()> {
    let mut runtime = TerminalRuntime::default();
    loop {
        if app.runtime_tick_before_data_drain(terminal)? {
            return Ok(());
        }
        runtime.drain_app_data_events(app);
        app.runtime_tick_after_data_drain();

        let view = runtime.view_for_render(app.current_app_view());
        // The production draw path consumes `AppView` end-to-end: the top
        // rule's mode badges are now derived from the seam, so the runtime
        // wiring carries real rendering data instead of being derived and
        // discarded.
        crate::ui::tui::render_app(terminal, &view, |frame| app.draw(frame, &view))?;
        app.on_frame_drawn();

        if let Some(command) = crate::ui::tui::poll_command(app.event_poll_duration(), &view)? {
            match runtime.route_command(command, &view) {
                TerminalCommandOutcome::HandledContinue => {}
                TerminalCommandOutcome::HandledExit => {
                    crate::runner::shutdown_all_runs();
                    return Ok(());
                }
                TerminalCommandOutcome::Legacy(command) => {
                    if app.handle_app_command(command) {
                        crate::runner::shutdown_all_runs();
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_runtime::{AgentRunSummary, AppCommand, AppView, ModalKind};
    use crate::data::events::{DataEvent, DataOutcome, DataRequest, LiveSummaryEvents};
    use crate::logic::pipeline::RunStatus;
    use std::sync::Arc;
    use std::sync::mpsc;

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
        let confirm =
            runtime.route_command_with_dispatch(AppCommand::ConfirmModal, &view, |request| {
                requests.push(request);
                DataOutcome::Terminated(true)
            });

        assert_eq!(confirm, TerminalCommandOutcome::HandledExit);
        assert_eq!(
            requests,
            vec![DataRequest::TerminateRun {
                window_name: "codexize-run-7-planning".to_string(),
            }]
        );
    }

    #[test]
    fn runtime_drains_live_summary_watcher_as_data_events() {
        let runtime = TerminalRuntime::default();
        let (tx, rx) = mpsc::channel();
        tx.send(()).expect("send watcher signal");
        tx.send(()).expect("send watcher signal");
        let events = LiveSummaryEvents::new(rx);

        assert_eq!(
            runtime.drain_live_summary_data_events(Some(&events)),
            vec![DataEvent::LiveSummaryChanged, DataEvent::LiveSummaryChanged]
        );
        assert_eq!(
            runtime.drain_live_summary_data_events(Some(&events)),
            vec![]
        );
    }
}
