use crate::app_runtime::frontend::{Frontend, FrontendConnector};
use crate::app_runtime::terminal::{TerminalCommandOutcome, TerminalRuntime};
use crate::app_runtime::{AppShell, ShellCommandOutcome};
use crate::ui::tui::{self, AppTerminal, CrosstermInputAdapter};
use crate::ui::widgets::sidebar::view::{render_sidebar, sidebar_width};
use anyhow::Result;
use ratatui::layout::Rect;

pub struct TerminalFrontend<'a> {
    pub shell: &'a mut AppShell,
    pub terminal: &'a mut AppTerminal,
}

impl Frontend for TerminalFrontend<'_> {
    fn run(self, connector: FrontendConnector) -> Result<()> {
        let mut app = self.shell.take_focused_app();
        let mut runtime = TerminalRuntime::default();
        let mut input = CrosstermInputAdapter::spawn();

        loop {
            if connector.shutdown.is_set() {
                app.drain_notifications_for_shutdown();
                self.shell.return_app_to_supervisor(app);
                return Ok(());
            }
            let _snapshot = connector.snapshot.read();
            // Transitional TUI rendering still reads the live AppShell/App
            // graph until the typed-command slice can drive it solely from
            // RootView; keep building the root projection here so this
            // frontend exercises the same seam without changing behavior.
            let _root_view = self.shell.current_root_view();
            while connector.events.try_recv().is_ok() {}

            // Park the focused App back inside its supervisor for the
            // scheduler tick. With the App lent back, the scheduler can
            // drive every session (focused or not) through the supervisor
            // map under a single code path, so the tick no longer needs
            // to borrow the focused `App` (spec §4.7, §4.8 line 280).
            self.shell.return_app_to_supervisor(app);
            if let Err(err) = self.shell.run_scheduler_tick() {
                tracing::warn!("scheduler tick failed: {err}");
            }
            app = self.shell.take_focused_app();
            if let Some(path) = app.take_pending_view_path() {
                input.shutdown_blocking();
                app.run_external_view_editor(&path, |run_editor| {
                    let _ = tui::run_foreground(self.terminal, || {
                        run_editor();
                        Ok(())
                    });
                });
                input = CrosstermInputAdapter::spawn();
            }
            if app.runtime_tick_before_data_drain() {
                app.drain_notifications_for_shutdown();
                self.shell.return_app_to_supervisor(app);
                return Ok(());
            }
            runtime.drain_app_data_events(&mut app);
            app.runtime_tick_after_data_drain();
            let mut view = runtime.view_for_render(app.current_app_view());
            let shell_view = self.shell.sidebar_view();
            view.shell_visible = shell_view.visible;
            view.shell_focus = shell_view.focus;

            tui::render_app(self.terminal, |frame| {
                let full_area = frame.area();
                if self.shell.sidebar_visible() {
                    let sidebar_w = sidebar_width().min(full_area.width.saturating_sub(20).max(10));
                    let sidebar_area =
                        Rect::new(full_area.x, full_area.y, sidebar_w, full_area.height);
                    let app_area = Rect::new(
                        full_area.x + sidebar_w,
                        full_area.y,
                        full_area.width.saturating_sub(sidebar_w),
                        full_area.height,
                    );
                    let sidebar_view = self.shell.sidebar_view();
                    render_sidebar(sidebar_area, frame.buffer_mut(), &sidebar_view);
                    app.draw_in_area(frame, &view, app_area);
                } else {
                    app.draw(frame, &view);
                }
            })?;

            app.on_frame_drawn();

            if let Some(command) = input.next_command(app.event_poll_duration(), &view)? {
                // Shell intercepts sidebar-navigation keys first.
                if self.shell.sidebar_visible() {
                    let modal_open = app.current_app_view().modal.is_some();
                    match self
                        .shell
                        .handle_shell_command(command.clone(), modal_open)?
                    {
                        ShellCommandOutcome::Consumed => {
                            app = self.shell.swap_focused_app_if_needed(app);
                            continue;
                        }
                        ShellCommandOutcome::Unhandled => {}
                    }
                }

                let outcome = runtime.route_command_with_dispatch(command, &view, |request| {
                    crate::data::events::dispatch(request, &app.runner_supervisor)
                });
                match outcome {
                    TerminalCommandOutcome::HandledContinue => {}
                    TerminalCommandOutcome::HandledExit => {
                        app.runner_supervisor.shutdown_all_runs();
                        app.drain_notifications_for_shutdown();
                        self.shell.return_app_to_supervisor(app);
                        connector.shutdown.set();
                        return Ok(());
                    }
                    TerminalCommandOutcome::AppOwned(command) => {
                        if app.handle_app_command(command) {
                            app.runner_supervisor.shutdown_all_runs();
                            app.drain_notifications_for_shutdown();
                            self.shell.return_app_to_supervisor(app);
                            connector.shutdown.set();
                            return Ok(());
                        }
                    }
                }

                // If the App executed a shell-level palette command, forward it.
                if let Some("sessions") = app.pending_shell_command.take().as_deref() {
                    let _ = self.shell.execute_shell_palette_command("sessions");
                }
            }
        }
    }
}
