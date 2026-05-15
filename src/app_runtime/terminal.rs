//! Production terminal runtime routing helpers.
//!
//! The TUI owns crossterm event collection and terminal drawing. This
//! module owns the UI-neutral routing that still belongs on the runtime
//! side: live-summary data-event draining and command dispatch decisions.
use crate::app::App;
use crate::app_runtime::{AppCommand, AppView, ModalKind};
use crate::data::events::{DataEvent, DataOutcome, DataRequest, LiveSummaryEvents};
use crate::state::RunStatus;
/// Result of routing an [`AppCommand`] through the terminal runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalCommandOutcome {
    /// Runtime handled the command and should keep running.
    HandledContinue,
    /// Runtime handled the command and should exit the app loop.
    HandledExit,
    /// Command is still owned by `App`.
    AppOwned(AppCommand),
}
/// Runtime-owned production state that is not part of `App`.
///
/// This keeps quit-confirmation state outside `App`: the UI emits commands,
/// app_runtime owns modal state and side-effect dispatch, and App is only asked
/// to handle commands that have not moved across the seam.
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
    pub(crate) fn drain_app_data_events(&mut self, app: &mut App) {
        let drained = self.drain_live_summary_data_events(app.live_summary_change_events.as_mut());
        if drained
            .iter()
            .any(|event| matches!(event, DataEvent::LiveSummaryChanged))
        {
            app.read_live_summary_pipeline();
        }
        app.poll_live_summary_mtime();
        // Cache watcher: external `models.json` publishes by another
        // instance flow into the redraw loop the same way live-summary
        // changes do — a single debounced reload per atomic rename.
        app.poll_cache_watcher();
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
            // by the runtime (`AppCommand::Quit`) or the App-owned `:quit`
            // command path (which sets only `view.modal`). Both emit
            // `ConfirmModal` from the UI; the runtime owns the termination
            // dispatch in either case so App never has to round-trip back
            // through the runtime.
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
            other => TerminalCommandOutcome::AppOwned(other),
        }
    }
}
#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
