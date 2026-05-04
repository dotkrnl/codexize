//! Production terminal runtime coordinator.
//!
//! The TUI owns crossterm event collection and terminal drawing, while this
//! module owns the application loop ordering: data/runtime tick, render, then
//! command dispatch.

use anyhow::Result;

use crate::{app::App, tui::AppTerminal};

/// Run the production terminal app through the app-runtime seam.
pub fn run_terminal_app(app: &mut App, terminal: &mut AppTerminal) -> Result<()> {
    loop {
        if app.runtime_tick_before_draw(terminal)? {
            return Ok(());
        }
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
