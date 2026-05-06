//! Production terminal runtime coordinator.
//!
//! The TUI owns crossterm event collection and terminal drawing, while this
//! module owns the application loop ordering: pre-drain tick, post-drain
//! tick, render, then command dispatch.
use crate::app_runtime::{AppCommand, AppView, ModalKind};
use crate::data::events::{DataEvent, DataOutcome, DataRequest, LiveSummaryEvents};
use crate::state::RunStatus;
use crate::{app::App, tui::AppTerminal};
use anyhow::Result;
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
        &mut self,
        events: Option<&mut LiveSummaryEvents>,
    ) -> Vec<DataEvent> {
        events.map(LiveSummaryEvents::drain).unwrap_or_default()
    }
    fn drain_app_data_events(&mut self, app: &mut App) {
        let drained = self.drain_live_summary_data_events(app.live_summary_change_events.as_mut());
        if drained
            .iter()
            .any(|event| matches!(event, DataEvent::LiveSummaryChanged))
        {
            app.read_live_summary_pipeline();
        }
        app.poll_live_summary_fallback();
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn route_command(
        &mut self,
        command: AppCommand,
        view: &AppView,
    ) -> TerminalCommandOutcome {
        let supervisor = crate::runner::Supervisor::shared_for_test();
        self.route_command_with_dispatch(command, view, |request| {
            crate::data::events::dispatch(request, &supervisor)
        })
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
            // Quit confirmation must converge whether the modal was opened
            // by the runtime (`AppCommand::Quit`) or the legacy App-owned
            // `:quit` command path (which sets only `view.modal`). Both
            // emit `ConfirmModal` from the UI; the runtime owns the
            // termination dispatch in either case so App never has to
            // round-trip back through the runtime.
            AppCommand::ConfirmModal
                if matches!(
                    self.modal_override.or(view.modal),
                    Some(ModalKind::QuitRunningAgent)
                ) =>
            {
                for run in view
                    .agent_runs
                    .iter()
                    .filter(|run| run.status == RunStatus::Running)
                {
                    let _ = dispatch(DataRequest::TerminateRun { run_id: run.id });
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
    let mut input = crate::ui::tui::CrosstermInputAdapter::spawn();
    loop {
        if app.runtime_tick_before_data_drain(terminal)? {
            app.drain_notifications_for_shutdown();
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
        if let Some(command) = input.next_command(app.event_poll_duration(), &view)? {
            let outcome = runtime.route_command_with_dispatch(command, &view, |request| {
                crate::data::events::dispatch(request, &app.runner_supervisor)
            });
            match outcome {
                TerminalCommandOutcome::HandledContinue => {}
                TerminalCommandOutcome::HandledExit => {
                    app.runner_supervisor.shutdown_all_runs();
                    app.drain_notifications_for_shutdown();
                    return Ok(());
                }
                TerminalCommandOutcome::Legacy(command) => {
                    if app.handle_app_command(command) {
                        app.runner_supervisor.shutdown_all_runs();
                        app.drain_notifications_for_shutdown();
                        return Ok(());
                    }
                }
            }
        }
    }
}
#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
