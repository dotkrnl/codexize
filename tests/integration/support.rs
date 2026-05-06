use std::path::{Path, PathBuf};
use std::sync::Arc;

use codexize::app_runtime::{
    AppCommand, AppView, ModalKind, StageId, StatusMessage, StatusSeverity,
};
use codexize::data::events::{DataOutcome, DataRequest, dispatch_observation};
use codexize::logic::rules::retry_phase_for_stage;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub fn drain_views(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppView>) -> Vec<AppView> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

pub struct UiChannels {
    pub commands_tx: UnboundedSender<AppCommand>,
    pub views_rx: UnboundedReceiver<AppView>,
}

pub struct RuntimeChannels {
    pub commands_rx: UnboundedReceiver<AppCommand>,
    pub views_tx: UnboundedSender<AppView>,
}

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
            AppCommand::Quit => self.view.modal = Some(ModalKind::QuitRunningAgent),
            AppCommand::OpenPalette | AppCommand::CancelModal => self.view.modal = None,
            AppCommand::ToggleYolo => self.view.modes.yolo = !self.view.modes.yolo,
            AppCommand::ToggleCheap => self.view.modes.cheap = !self.view.modes.cheap,
            AppCommand::RetryStage(stage) => {
                if let Some(phase) = retry_phase_for_stage(stage_name(stage)) {
                    self.view.phase = phase;
                    self.view.modal = Some(ModalKind::StageError(stage));
                }
            }
            AppCommand::SubmitInput { text } => self.apply_live_summary_status(&text),
            AppCommand::DismissStatus => self.view.status = None,
            _ => {}
        }
        if is_quit {
            RuntimeControl::Exit
        } else {
            RuntimeControl::Continue
        }
    }

    fn apply_live_summary_status(&mut self, fallback: &str) {
        let outcome = dispatch_observation(&DataRequest::ReadLiveSummary {
            path: self.live_summary_path.clone(),
        })
        .expect("ReadLiveSummary is an observation-only variant");
        let DataOutcome::LiveSummaryRead(snapshot) = outcome else {
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
        StageId::FinalValidation => "final-validation",
        StageId::Dreaming => "dreaming",
    }
}

pub fn run_headless_until_exit(
    runtime: &mut HeadlessRuntime,
    mut channels: RuntimeChannels,
) -> anyhow::Result<RuntimeControl> {
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

pub fn headless_runtime_for_live_summary(
    session_id: impl Into<Arc<str>>,
    live_summary_path: impl AsRef<Path>,
) -> HeadlessRuntime {
    HeadlessRuntime::new(
        AppView::empty(session_id),
        live_summary_path.as_ref().to_path_buf(),
    )
}

pub fn run_harness_until_exit(
    harness: &mut RuntimeHarness,
    mut runtime: RuntimeChannels,
) -> anyhow::Result<RuntimeControl> {
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
