//! Headless runtime and stubbed-UI harness for the [`AppView`]/[`AppCommand`] seam.
//!
//! The seam between [`crate::app_runtime`] and [`crate::ui`] is two
//! channels: the runtime publishes [`AppView`] snapshots, the UI emits
//! [`AppCommand`] back. This harness gives tests (and future server-mode
//! callers) a pair of typed channels they can wire up without any
//! terminal or rendering dependencies.
//!
//! Today the production TUI still owns some state directly via
//! [`crate::app::App`], but this headless runner owns its [`AppView`]
//! state inside `app_runtime` and routes representative commands through
//! real logic and data entrypoints. That gives integration tests and
//! future server-mode callers a runtime-owned surface without touching
//! `ratatui`/`crossterm`.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{AppCommand, AppView, ModalKind, StageId, StatusMessage, StatusSeverity};
use crate::data::events::{DataOutcome, DataRequest, dispatch_observation};
use crate::logic::rules::retry_phase_for_stage;

/// UI side of the runtime seam. The UI sends operator intent and reads
/// derived snapshots; it does not have access to runtime-internal state.
pub struct UiChannels {
    pub commands_tx: UnboundedSender<AppCommand>,
    pub views_rx: UnboundedReceiver<AppView>,
}

/// Runtime side of the seam. The runtime drains commands and publishes
/// the next view after applying each command.
pub struct RuntimeChannels {
    pub commands_rx: UnboundedReceiver<AppCommand>,
    pub views_tx: UnboundedSender<AppView>,
}

/// Build a paired set of channels for the runtime and a stubbed UI. The
/// pair is the headless equivalent of `ui/runtime` setting up the
/// terminal — same shape, no IO.
pub fn channel_pair() -> (UiChannels, RuntimeChannels) {
    let (commands_tx, commands_rx) = unbounded_channel();
    let (views_tx, views_rx) = unbounded_channel();
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

/// Runtime-owned, terminal-free event pump for tests and future non-TUI UIs.
///
/// The runner owns the authoritative [`AppView`], consumes [`AppCommand`]s
/// from the UI channel, routes domain decisions through [`crate::logic`],
/// dispatches side-effect reads through [`crate::data`], then publishes the
/// resulting snapshot. Tests should assert on the published snapshots, not
/// mutate or recompute a local view.
pub struct HeadlessRuntime {
    view: AppView,
    live_summary_path: PathBuf,
}

impl HeadlessRuntime {
    pub fn new(view: AppView, live_summary_path: impl Into<PathBuf>) -> Self {
        Self {
            view,
            live_summary_path: live_summary_path.into(),
        }
    }

    pub fn view(&self) -> &AppView {
        &self.view
    }

    fn apply_command(&mut self, command: AppCommand) -> RuntimeControl {
        let is_quit = matches!(command, AppCommand::Quit);
        match command {
            AppCommand::Quit => {
                self.view.modal = Some(ModalKind::QuitRunningAgent);
            }
            AppCommand::OpenPalette | AppCommand::CancelModal => {
                self.view.modal = None;
            }
            AppCommand::ToggleYolo => {
                self.view.modes.yolo = !self.view.modes.yolo;
            }
            AppCommand::ToggleCheap => {
                self.view.modes.cheap = !self.view.modes.cheap;
            }
            AppCommand::RetryStage(stage) => {
                if let Some(phase) = retry_phase_for_stage(stage_name(stage)) {
                    self.view.phase = phase;
                    self.view.modal = Some(ModalKind::StageError(stage));
                }
            }
            AppCommand::SubmitInput { text } => {
                self.apply_live_summary_status(&text);
            }
            AppCommand::DismissStatus => {
                self.view.status = None;
            }
            _ => {}
        }
        if is_quit {
            RuntimeControl::Exit
        } else {
            RuntimeControl::Continue
        }
    }

    fn apply_live_summary_status(&mut self, fallback: &str) {
        // The live-summary read is observation-only; routing it through
        // `dispatch_observation` keeps the headless runtime free of an unused
        // `Supervisor` argument that future readers might assume is load-bearing.
        let outcome = dispatch_observation(&DataRequest::ReadLiveSummary {
            path: self.live_summary_path.clone(),
        })
        .expect("ReadLiveSummary is an observation-only variant");
        let DataOutcome::LiveSummaryRead(snapshot) = outcome else {
            // Keep this explicit so future DataRequest rewrites cannot
            // silently publish a status for the wrong side-effect outcome.
            panic!("ReadLiveSummary must return LiveSummaryRead");
        };
        self.view.status = Some(match snapshot {
            Some(snap) => StatusMessage {
                text: Arc::from(snap.content.as_str()),
                severity: StatusSeverity::Info,
            },
            None => StatusMessage {
                text: Arc::from(fallback),
                severity: StatusSeverity::Warn,
            },
        });
    }
}

fn stage_name(stage: StageId) -> &'static str {
    match stage {
        StageId::Brainstorm => "brainstorm",
        StageId::SpecReview => "spec-review",
        StageId::Planning => "planning",
        StageId::PlanReview => "plan-review",
        StageId::Sharding => "sharding",
        StageId::Implementation => "implementation",
        StageId::Review => "review",
    }
}

/// Run a headless app runtime until it drains commands or receives quit.
///
/// An initial view is published before command processing starts. Every
/// drained command publishes exactly one subsequent runtime-owned snapshot.
pub fn run_headless_until_exit(
    runtime: &mut HeadlessRuntime,
    mut channels: RuntimeChannels,
) -> Result<RuntimeControl> {
    channels.views_tx.send(runtime.view.clone())?;
    let mut control = RuntimeControl::Continue;
    while let Ok(command) = channels.commands_rx.try_recv() {
        control = runtime.apply_command(command);
        channels.views_tx.send(runtime.view.clone())?;
        if control == RuntimeControl::Exit {
            break;
        }
    }
    Ok(control)
}

/// Convenience helper for tests that need a real data dispatch path.
pub fn headless_runtime_for_live_summary(
    session_id: impl Into<Arc<str>>,
    live_summary_path: impl AsRef<Path>,
) -> HeadlessRuntime {
    HeadlessRuntime::new(
        AppView::empty(session_id),
        live_summary_path.as_ref().to_path_buf(),
    )
}

/// Drive the app-runtime command/view seam without terminal UI.
pub fn run_harness_until_exit(
    harness: &mut RuntimeHarness,
    mut runtime: RuntimeChannels,
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
    use crate::logic::pipeline::Phase;
    use tempfile::tempdir;

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
        let (mut ui, mut runtime) = channel_pair();

        // Seed an initial view as the runtime would.
        let mut view = AppView::empty("session-stub");
        runtime
            .views_tx
            .send(view.clone())
            .expect("publish initial view");

        let initial = ui
            .views_rx
            .blocking_recv()
            .expect("ui receives initial view");
        assert_eq!(initial.phase, Phase::IdeaInput);
        assert!(initial.modal.is_none());

        // UI emits a command; runtime drains and republishes.
        ui.commands_tx
            .send(AppCommand::Quit)
            .expect("ui sends quit");
        let received = runtime
            .commands_rx
            .blocking_recv()
            .expect("runtime drains command");
        assert_eq!(received, AppCommand::Quit);
        stub_runtime_step(&mut view, received);
        runtime
            .views_tx
            .send(view.clone())
            .expect("publish updated view");

        let after_quit = ui
            .views_rx
            .blocking_recv()
            .expect("ui receives updated view");
        assert_eq!(after_quit.modal, Some(ModalKind::QuitRunningAgent));
    }

    #[test]
    fn runtime_can_drive_a_full_round_trip_without_ui_state() {
        let (ui, mut runtime) = channel_pair();
        let mut views_rx = ui.views_rx;
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
            let received = runtime.commands_rx.blocking_recv().expect("runtime drains");
            stub_runtime_step(&mut view, received);
            runtime.views_tx.send(view.clone()).expect("publish view");
            let _ = views_rx.blocking_recv().expect("ui receives view");
        }

        // After the script the modal returns to None — proves command
        // routing is bidirectional and pure-value.
        assert!(view.modal.is_none());
    }

    #[test]
    fn headless_runtime_routes_logic_and_data_before_publishing() {
        let dir = tempdir().expect("tempdir");
        let live_summary_path = dir.path().join("live.txt");
        std::fs::write(&live_summary_path, "runtime-owned").expect("seed");
        let (mut ui, channels) = channel_pair();
        ui.commands_tx
            .send(AppCommand::RetryStage(StageId::Brainstorm))
            .expect("send retry");
        ui.commands_tx
            .send(AppCommand::SubmitInput {
                text: "fallback".to_string(),
            })
            .expect("send input");
        ui.commands_tx.send(AppCommand::Quit).expect("send quit");

        let mut runtime = headless_runtime_for_live_summary("headless", &live_summary_path);
        let control = run_headless_until_exit(&mut runtime, channels).expect("run");

        assert_eq!(control, RuntimeControl::Exit);
        assert_eq!(runtime.view().phase, Phase::BrainstormRunning);
        let snapshots: Vec<_> = drain_views(&mut ui.views_rx);
        assert_eq!(snapshots.len(), 4);
        assert_eq!(snapshots[1].phase, Phase::BrainstormRunning);
        assert_eq!(
            snapshots[2].status.as_ref().unwrap().text.as_ref(),
            "runtime-owned"
        );
        assert_eq!(snapshots[3].modal, Some(ModalKind::QuitRunningAgent));
    }

    fn drain_views(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppView>) -> Vec<AppView> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }
}
