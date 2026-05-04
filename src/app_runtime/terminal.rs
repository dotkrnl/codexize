//! Production terminal runtime coordinator.
//!
//! The TUI owns crossterm event collection and terminal drawing, while this
//! module owns the application loop ordering: pre-drain tick, drain
//! [`DataEvent`]s and route them per-event, post-drain tick, render, then
//! command dispatch.

use anyhow::Result;

use crate::data::events::DataEvent;
use crate::data::runner;
use crate::{app::App, tui::AppTerminal};

/// Drain queued tool-call transitions from `data/runner` and route each
/// one through the per-event app handler. The runtime owns the drain so
/// the coordinator (rather than `App`) consumes [`DataEvent`] values.
fn drain_tool_call_transitions(app: &mut App) {
    for event in runner::drain_tool_call_events() {
        let DataEvent::ToolCallTransition {
            window_name,
            transition,
        } = event
        else {
            continue;
        };
        app.apply_tool_call_transition(&window_name, transition);
    }
}

/// Run the production terminal app through the app-runtime seam.
pub fn run_terminal_app(app: &mut App, terminal: &mut AppTerminal) -> Result<()> {
    loop {
        if app.runtime_tick_before_data_drain(terminal)? {
            return Ok(());
        }
        drain_tool_call_transitions(app);
        app.runtime_tick_after_data_drain();

        let view = app.current_app_view();
        // The production draw path consumes `AppView` end-to-end: the top
        // rule's mode badges are now derived from the seam, so the runtime
        // wiring carries real rendering data instead of being derived and
        // discarded.
        crate::ui::tui::render_app(terminal, &view, |frame| app.draw(frame, &view))?;
        app.on_frame_drawn();

        if let Some(command) = crate::ui::tui::poll_command(app.event_poll_duration())?
            && app.handle_app_command(command)
        {
            crate::runner::shutdown_all_runs();
            return Ok(());
        }
    }
}
