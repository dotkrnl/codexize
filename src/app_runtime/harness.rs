//! Stubbed-UI harness exercising the [`AppView`]/[`AppCommand`] seam.
//!
//! The seam between [`crate::app_runtime`] and [`crate::ui`] is two
//! channels: the runtime publishes [`AppView`] snapshots, the UI emits
//! [`AppCommand`] back. This harness gives tests (and future server-mode
//! callers) a pair of typed channels they can wire up without any
//! terminal or rendering dependencies.
//!
//! Today the production TUI still owns the state directly via
//! [`crate::app::App`]; the harness proves the seam is real and works
//! end-to-end without touching `ratatui`/`crossterm`. The next slice of
//! the refactor will switch the production runtime to publish through
//! [`UiChannels::views_tx`] instead of mutating the TUI struct in place.

use anyhow::Result;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::{AppCommand, AppView};

/// UI side of the runtime seam. The UI sends operator intent and reads
/// derived snapshots; it does not have access to runtime-internal state.
pub struct UiChannels {
    pub commands_tx: Sender<AppCommand>,
    pub views_rx: Receiver<AppView>,
}

/// Runtime side of the seam. The runtime drains commands and publishes
/// the next view after applying each command.
pub struct RuntimeChannels {
    pub commands_rx: Receiver<AppCommand>,
    pub views_tx: Sender<AppView>,
}

/// Build a paired set of channels for the runtime and a stubbed UI. The
/// pair is the headless equivalent of `ui/runtime` setting up the
/// terminal — same shape, no IO.
pub fn channel_pair() -> (UiChannels, RuntimeChannels) {
    let (commands_tx, commands_rx) = channel();
    let (views_tx, views_rx) = channel();
    (
        UiChannels {
            commands_tx,
            views_rx,
        },
        RuntimeChannels {
            commands_rx,
            views_tx,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeControl {
    Continue,
    Exit,
}

pub struct RuntimeHarness {
    view: AppView,
    commands: Vec<AppCommand>,
}

impl RuntimeHarness {
    pub fn new(view: AppView) -> Self {
        Self {
            view,
            commands: Vec::new(),
        }
    }

    pub fn commands(&self) -> &[AppCommand] {
        &self.commands
    }

    fn apply_command(&mut self, command: AppCommand) -> RuntimeControl {
        let control = if matches!(command, AppCommand::Quit) {
            RuntimeControl::Exit
        } else {
            RuntimeControl::Continue
        };
        self.commands.push(command);
        control
    }
}

/// Drive the app-runtime command/view seam without terminal UI.
pub fn run_harness_until_exit(
    harness: &mut RuntimeHarness,
    runtime: RuntimeChannels,
) -> Result<RuntimeControl> {
    let mut control = RuntimeControl::Continue;
    while let Ok(command) = runtime.commands_rx.try_recv() {
        control = harness.apply_command(command);
        runtime.views_tx.send(harness.view.clone())?;
        if control == RuntimeControl::Exit {
            break;
        }
    }
    Ok(control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_runtime::view::{ModalKind, StageId};
    use crate::logic::pipeline::Phase;

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
        let (ui, runtime) = channel_pair();

        // Seed an initial view as the runtime would.
        let mut view = AppView::empty("session-stub");
        runtime
            .views_tx
            .send(view.clone())
            .expect("publish initial view");

        let initial = ui.views_rx.recv().expect("ui receives initial view");
        assert_eq!(initial.phase, Phase::IdeaInput);
        assert!(initial.modal.is_none());

        // UI emits a command; runtime drains and republishes.
        ui.commands_tx
            .send(AppCommand::Quit)
            .expect("ui sends quit");
        let received = runtime.commands_rx.recv().expect("runtime drains command");
        assert_eq!(received, AppCommand::Quit);
        stub_runtime_step(&mut view, received);
        runtime
            .views_tx
            .send(view.clone())
            .expect("publish updated view");

        let after_quit = ui.views_rx.recv().expect("ui receives updated view");
        assert_eq!(after_quit.modal, Some(ModalKind::QuitRunningAgent));
    }

    #[test]
    fn runtime_can_drive_a_full_round_trip_without_ui_state() {
        let (ui, runtime) = channel_pair();
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
            let received = runtime.commands_rx.recv().expect("runtime drains");
            stub_runtime_step(&mut view, received);
            runtime.views_tx.send(view.clone()).expect("publish view");
            let _ = ui.views_rx.recv().expect("ui receives view");
        }

        // After the script the modal returns to None — proves command
        // routing is bidirectional and pure-value.
        assert!(view.modal.is_none());
    }
}
