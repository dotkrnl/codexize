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
        // The production TUI still renders through the legacy `App` frame
        // closure; deriving `AppView` here keeps the runtime path aligned with
        // the channel seam until widget rendering consumes the view directly.
        crate::ui::tui::render_app(terminal, &view, |frame| app.draw(frame))?;
        app.on_frame_drawn();

        if let Some(command) = crate::ui::tui::poll_command(app.event_poll_duration())?
            && app.handle_app_command(command)
        {
            crate::runner::shutdown_all_runs();
            return Ok(());
        }
    }
}
