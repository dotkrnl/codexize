//! Runtime entrypoint helper that builds the frontend seam and hands it
//! off to a concrete [`Frontend`].
//!
//! Concretely, [`run_frontend`] is responsible for the five wiring steps
//! pinned by the spec (Milestone 1 Stage E):
//!
//! 1. Construct an `Arc<RwLock<RootView>>` seeded with the initial state.
//! 2. Build a `std::sync::mpsc` event channel and command channel.
//! 3. Assemble a [`FrontendConnector`] (snapshot, events, commands,
//!    shutdown) over those.
//! 4. Emit `RootEventPayload::Snapshot(RootView)` exactly once before any
//!    granular delta, so a frontend can initialize from events alone.
//! 5. Hand off to `frontend.run(connector)`.
//!
//! The state-change update loop that publishes granular `RootEvent`s
//! lands in later tasks — today's terminal frontend still mutates `App`
//! internal state directly so no operator-visible behavior changes. The
//! `seq`-before-publish ordering invariant is encoded in [`publish`] and
//! enforced as soon as the runtime starts emitting granular deltas.
use super::commands::{
    AppCommand, GlobalCommand, InputCommand, ModalAction, ModalCommand, PaletteCommand,
    SessionCommand, StageCommand, TreeCommand,
};
use super::frontend::{Frontend, FrontendConnector, ShutdownSignal, SnapshotHandle};
use super::root_view::{RootEvent, RootEventPayload, RootView, SessionId, SessionViewDelta};
use super::views::chat::{ChatMessage, ChatMessageKind};
use super::views::palette::PaletteView;
use super::views::session::SessionView;
use super::views::shell::SidebarRow;
use super::views::tree::{TreeNodeStatus, TreeView, VisibleNodeRow};
use crate::app_runtime::StageId;
use crate::logic::pipeline::Stage;
use anyhow::Result;
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

type PublishResult = std::result::Result<(), Box<mpsc::SendError<RootEvent>>>;

/// Build a fresh `FrontendConnector` (and a writer-side handle on the
/// snapshot) for use by [`run_frontend`] and tests.
pub fn build_connector() -> (FrontendConnector, RuntimePublisher) {
    let snapshot_inner = Arc::new(RwLock::new(RootView::initial()));
    let snapshot = SnapshotHandle::new(Arc::clone(&snapshot_inner));
    let (event_tx, event_rx) = mpsc::channel::<RootEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let shutdown = ShutdownSignal::new();
    let connector = FrontendConnector {
        snapshot,
        events: event_rx,
        commands: command_tx,
        shutdown: shutdown.clone(),
    };
    let publisher = RuntimePublisher {
        snapshot: snapshot_inner,
        events: event_tx,
        commands: command_rx,
        shutdown,
    };
    (connector, publisher)
}

/// Runtime-side handle on the snapshot, event channel, and inbound command
/// stream. Owns the write lock; frontends only ever see the read-side
/// `SnapshotHandle` through the `FrontendConnector`.
///
/// Later tasks attach the runtime update loop to this struct so every
/// state change writes the snapshot under [`publish`] and the matching
/// `RootEvent` is sent on the event channel. The exact loop wiring is
/// out of scope for this task.
pub struct RuntimePublisher {
    snapshot: Arc<RwLock<RootView>>,
    events: mpsc::Sender<RootEvent>,
    #[allow(dead_code)] // wired by later tasks; preserved here so the
    // command receiver isn't dropped (which would close the channel and
    // make every frontend `commands.send(..)` fail).
    commands: mpsc::Receiver<AppCommand>,
    shutdown: ShutdownSignal,
}

impl RuntimePublisher {
    /// Atomically apply `mutate` to the current `RootView`, bump `seq`,
    /// then publish `event_for(seq)` on the event channel. The write
    /// completes before the publish, so the spec's
    /// `snapshot.read().seq >= event.seq` invariant holds for every
    /// receiver.
    ///
    /// Returns `Err` only if the event channel is closed (no frontend is
    /// listening); callers may treat that as a benign shutdown signal.
    pub fn publish<F, E>(&self, mutate: F, event_for: E) -> PublishResult
    where
        F: FnOnce(&mut RootView),
        E: FnOnce(u64) -> RootEventPayload,
    {
        let seq = {
            let mut guard = self.snapshot.write();
            mutate(&mut guard);
            guard.seq = guard.seq.saturating_add(1);
            guard.seq
        };
        self.events
            .send(RootEvent {
                seq,
                payload: event_for(seq),
            })
            .map_err(Box::new)
    }

    /// Emit the initial `Snapshot` payload. Used by [`run_frontend`] before
    /// handing the connector to the frontend so a frontend can rely on
    /// receiving exactly one `Snapshot` before any granular delta.
    pub fn emit_initial_snapshot(&self) -> PublishResult {
        // The seeded `RootView` already has seq = 0; the event carries the
        // same seq so the spec's "match" invariant holds at startup too.
        let view = self.snapshot.read().clone();
        self.events
            .send(RootEvent {
                seq: view.seq,
                payload: RootEventPayload::Snapshot(view),
            })
            .map_err(Box::new)
    }

    pub fn shutdown(&self) -> ShutdownSignal {
        self.shutdown.clone()
    }

    pub fn spawn_command_loop(self) -> thread::JoinHandle<()> {
        thread::spawn(move || self.run_command_loop())
    }

    fn run_command_loop(self) {
        while !self.shutdown.is_set() {
            match self.commands.recv_timeout(Duration::from_millis(25)) {
                Ok(command) => self.handle_command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn handle_command(&self, command: AppCommand) {
        match command {
            AppCommand::Global(GlobalCommand::Quit) => self.shutdown.set(),
            AppCommand::Session(session_id, command) => {
                self.handle_session_command(session_id, command)
            }
            _ => {}
        }
    }

    fn handle_session_command(&self, session_id: SessionId, command: SessionCommand) {
        match command {
            SessionCommand::Stage(command) => self.apply_stage_command(session_id, command),
            SessionCommand::Palette(command) => self.apply_palette_command(session_id, command),
            SessionCommand::Input(command) => self.apply_input_command(session_id, command),
            SessionCommand::Tree(command) => self.apply_tree_command(session_id, command),
            SessionCommand::Modal(ModalCommand::Action(ModalAction::RetryStage(stage_id))) => {
                self.apply_stage_command(session_id, StageCommand::Retry(stage_id));
            }
            SessionCommand::SubmitInput { text } => self.append_user_input(session_id, text),
            _ => {}
        }
    }

    fn apply_stage_command(&self, session_id: SessionId, command: StageCommand) {
        self.ensure_session(&session_id);
        let current_stage = self
            .snapshot
            .read()
            .sessions
            .get(&session_id)
            .map(|session| session.stage)
            .unwrap_or_default();
        let Some(next_stage) = stage_after_command(current_stage, command) else {
            return;
        };
        let event_session_id = Arc::clone(&session_id);
        let _ = self.publish(
            |view| {
                let session = view.sessions.entry(Arc::clone(&session_id)).or_default();
                session.stage = next_stage;
                view.focus = Arc::clone(&session_id);
                upsert_sidebar_row(view, &session_id, next_stage);
            },
            move |_| {
                RootEventPayload::SessionChanged(
                    event_session_id,
                    SessionViewDelta::Stage(next_stage),
                )
            },
        );
    }

    fn ensure_session(&self, session_id: &SessionId) {
        if self.snapshot.read().sessions.contains_key(session_id) {
            return;
        }
        let new_session_id = Arc::clone(session_id);
        let event_session_id = Arc::clone(session_id);
        let mut session = SessionView {
            stage: Stage::IdeaInput,
            ..SessionView::default()
        };
        let event_session = session.clone();
        let _ = self.publish(
            |view| {
                view.focus = Arc::clone(&new_session_id);
                view.sessions
                    .insert(Arc::clone(&new_session_id), std::mem::take(&mut session));
                upsert_sidebar_row(view, &new_session_id, Stage::IdeaInput);
            },
            move |_| RootEventPayload::SessionAdded(event_session_id, event_session),
        );
    }

    fn apply_palette_command(&self, session_id: SessionId, command: PaletteCommand) {
        self.ensure_session(&session_id);
        let Some(palette) = next_palette_view(
            self.snapshot
                .read()
                .sessions
                .get(&session_id)
                .map(|session| session.palette.clone())
                .unwrap_or_default(),
            command,
        ) else {
            return;
        };
        let event_session_id = Arc::clone(&session_id);
        let event_palette = palette.clone();
        let _ = self.publish(
            |view| {
                let session = view.sessions.entry(Arc::clone(&session_id)).or_default();
                session.palette = palette;
            },
            move |_| {
                RootEventPayload::SessionChanged(
                    event_session_id,
                    SessionViewDelta::Palette(event_palette),
                )
            },
        );
    }

    fn apply_input_command(&self, session_id: SessionId, command: InputCommand) {
        match command {
            InputCommand::Submit => {
                let text = self
                    .snapshot
                    .read()
                    .sessions
                    .get(&session_id)
                    .map(|session| session.palette.input_buffer.to_string())
                    .unwrap_or_default();
                if !text.is_empty() {
                    self.append_user_input(session_id, text);
                }
            }
            InputCommand::ReplaceBuffer(text) | InputCommand::InsertText(text) => {
                self.apply_palette_command(
                    session_id,
                    PaletteCommand::Edit(InputCommand::ReplaceBuffer(text)),
                );
            }
            _ => {}
        }
    }

    fn apply_tree_command(&self, session_id: SessionId, command: TreeCommand) {
        self.ensure_session(&session_id);
        let current = self
            .snapshot
            .read()
            .sessions
            .get(&session_id)
            .map(|session| session.tree.clone())
            .unwrap_or_default();
        let Some(tree) = next_tree_view(current, command) else {
            return;
        };
        let event_session_id = Arc::clone(&session_id);
        let event_tree = tree.clone();
        let _ = self.publish(
            |view| {
                let session = view.sessions.entry(Arc::clone(&session_id)).or_default();
                session.tree = tree;
            },
            move |_| {
                RootEventPayload::SessionChanged(
                    event_session_id,
                    SessionViewDelta::Tree(event_tree),
                )
            },
        );
    }

    fn append_user_input(&self, session_id: SessionId, text: String) {
        self.ensure_session(&session_id);
        let mut chat = self
            .snapshot
            .read()
            .sessions
            .get(&session_id)
            .map(|session| session.chat.clone())
            .unwrap_or_default();
        let mut messages = chat.messages.to_vec();
        messages.push(ChatMessage {
            kind: ChatMessageKind::UserInput,
            content: Arc::from(text),
            timestamp: Arc::from("headless"),
        });
        chat.messages = Arc::from(messages.into_boxed_slice());
        chat.follow_tail = true;
        let event_session_id = Arc::clone(&session_id);
        let event_chat = chat.clone();
        let _ = self.publish(
            |view| {
                let session = view.sessions.entry(Arc::clone(&session_id)).or_default();
                session.chat = chat;
            },
            move |_| {
                RootEventPayload::SessionChanged(
                    event_session_id,
                    SessionViewDelta::Chat(event_chat),
                )
            },
        );
    }
}

fn stage_after_command(current: Stage, command: StageCommand) -> Option<Stage> {
    match command {
        StageCommand::Start => Some(match current {
            Stage::IdeaInput => Stage::BrainstormRunning,
            Stage::SpecReviewPaused => Stage::PlanningRunning,
            Stage::PlanReviewPaused => Stage::RepoStateUpdateRunning,
            Stage::WaitingToImplement => Stage::RepoStateUpdateRunning,
            stage => stage,
        }),
        StageCommand::Approve => Some(match current {
            Stage::SpecReviewPaused => Stage::PlanningRunning,
            Stage::PlanReviewPaused => Stage::RepoStateUpdateRunning,
            stage => stage,
        }),
        StageCommand::Reject => Some(match current {
            Stage::SpecReviewPaused => Stage::BrainstormRunning,
            Stage::PlanReviewPaused => Stage::PlanningRunning,
            stage => stage,
        }),
        StageCommand::Retry(stage_id) => Some(stage_for_retry(stage_id)),
        StageCommand::GoBack => None,
    }
}

fn stage_for_retry(stage_id: StageId) -> Stage {
    match stage_id {
        StageId::Brainstorm => Stage::BrainstormRunning,
        StageId::SpecReview => Stage::SpecReviewRunning,
        StageId::Planning => Stage::PlanningRunning,
        StageId::PlanReview => Stage::PlanReviewRunning,
        StageId::RepoStateUpdate => Stage::RepoStateUpdateRunning,
        StageId::Sharding => Stage::ShardingRunning,
        StageId::Implementation => Stage::ImplementationRound(1),
        StageId::Recovery => Stage::BuilderRecovery(1),
        StageId::RecoveryPlanReview => Stage::BuilderRecoveryPlanReview(1),
        StageId::RecoverySharding => Stage::BuilderRecoverySharding(1),
        StageId::Review => Stage::ReviewRound(1),
        StageId::Simplification => Stage::Simplification(1),
        StageId::FinalValidation => Stage::FinalValidation(1),
        StageId::Dreaming => Stage::Dreaming(1),
    }
}

fn next_palette_view(mut palette: PaletteView, command: PaletteCommand) -> Option<PaletteView> {
    match command {
        PaletteCommand::Open => palette.is_open = true,
        PaletteCommand::Close { .. } => palette.is_open = false,
        PaletteCommand::Edit(InputCommand::ReplaceBuffer(text)) => {
            palette.input_buffer = Arc::from(text);
        }
        PaletteCommand::Edit(InputCommand::InsertText(text)) => {
            let mut input = palette.input_buffer.to_string();
            input.push_str(&text);
            palette.input_buffer = Arc::from(input);
        }
        PaletteCommand::Submit | PaletteCommand::AcceptGhost | PaletteCommand::Edit(_) => {
            return None;
        }
    }
    Some(palette)
}

fn next_tree_view(mut tree: TreeView, command: TreeCommand) -> Option<TreeView> {
    if tree.rows.is_empty() {
        tree.rows = Arc::from(
            vec![VisibleNodeRow {
                depth: 0,
                label: Arc::from("Stage"),
                status: TreeNodeStatus::Running,
                has_children: false,
                is_expanded: false,
                run_id: None,
            }]
            .into_boxed_slice(),
        );
    }
    let selected = tree.selected_index.unwrap_or(0);
    let row_count = tree.rows.len();
    match command {
        TreeCommand::MoveFocus { delta } | TreeCommand::ScrollOrMoveFocus { delta } => {
            let next = selected
                .saturating_add_signed(delta)
                .min(row_count.saturating_sub(1));
            tree.selected_index = Some(next);
        }
        TreeCommand::ScrollViewportPage { delta } => {
            let next = selected
                .saturating_add_signed(delta.saturating_mul(5))
                .min(row_count.saturating_sub(1));
            tree.selected_index = Some(next);
        }
        TreeCommand::ToggleExpand | TreeCommand::ActivateFocused => return None,
    }
    Some(tree)
}

fn upsert_sidebar_row(view: &mut RootView, session_id: &SessionId, stage: Stage) {
    let session_id_text = session_id.to_string();
    if let Some(row) = view
        .shell
        .rows
        .iter_mut()
        .find(|row| row.session_id == session_id_text)
    {
        row.stage = stage;
        row.focused = true;
        row.open = true;
        return;
    }
    view.shell.rows.push(SidebarRow {
        session_id: session_id_text,
        date_label: "headless".to_string(),
        title: stage.to_string(),
        stage,
        focused: true,
        open: true,
        running: !matches!(
            stage,
            Stage::IdeaInput
                | Stage::SpecReviewPaused
                | Stage::PlanReviewPaused
                | Stage::WaitingToImplement
                | Stage::SkipToImplPending
                | Stage::GitGuardPending
                | Stage::DreamingPending
                | Stage::Done
                | Stage::Cancelled
                | Stage::BlockedNeedsUser
        ),
    });
}

/// Build the frontend seam, emit the initial `Snapshot`, then hand off to
/// `frontend.run(connector)`.
///
/// The command loop is spawned in the background and joined after the
/// frontend exits, ensuring that the command receiver and snapshot writer
/// stay alive for the duration of the run.
pub fn run_frontend<F: Frontend>(frontend: F) -> Result<()> {
    let (connector, publisher) = build_connector();
    // Exactly one initial Snapshot before any granular delta (spec §
    // "Initialization flow"). A closed event channel here means no one is
    // listening yet, which is fine — the channel is consumed by the
    // frontend below.
    let _ = publisher.emit_initial_snapshot();
    let shutdown = publisher.shutdown();
    // Transitional headless support handles only typed seam commands here;
    // the full AppShell-backed runtime remains owned by the terminal path.
    let command_loop = publisher.spawn_command_loop();
    let result = frontend.run(connector);
    shutdown.set();
    let _ = command_loop.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_runtime::commands::{
        InputCommand, ModalAction, ModalCommand, PaletteCommand, TreeCommand,
    };
    use crate::app_runtime::frontend::RecordingFrontend;
    use crate::app_runtime::views::chat::ChatMessageKind;
    use crate::app_runtime::views::modal::StageId;
    use std::sync::Mutex;

    #[test]
    fn initial_snapshot_is_emitted_before_handoff() {
        let (connector, publisher) = build_connector();
        publisher.emit_initial_snapshot().unwrap();
        let event = connector.events.try_recv().expect("snapshot event");
        assert_eq!(event.seq, 0);
        match event.payload {
            RootEventPayload::Snapshot(view) => assert_eq!(view.seq, 0),
            other => panic!("expected Snapshot, got {other:?}"),
        }
        // Snapshot read agrees with the event's seq (spec invariant).
        assert!(connector.snapshot.read().seq >= event.seq);
    }

    #[test]
    fn publish_writes_snapshot_before_event() {
        let (connector, publisher) = build_connector();
        publisher
            .publish(
                |view| {
                    view.focus = Arc::<str>::from("alpha");
                },
                |_seq| RootEventPayload::FocusChanged(Arc::<str>::from("alpha")),
            )
            .unwrap();
        let event = connector.events.try_recv().expect("focus event");
        assert_eq!(event.seq, 1);
        let snap = connector.snapshot.read();
        assert!(snap.seq >= event.seq);
        assert_eq!(&*snap.focus, "alpha");
    }

    #[test]
    fn recording_frontend_drives_runtime_only_workflow_updates() {
        let session_id: SessionId = Arc::from("runtime-seam");
        let recorded_events = Arc::new(Mutex::new(Vec::new()));
        let frontend = RecordingFrontend {
            recorded_events: Arc::clone(&recorded_events),
            scripted_commands: vec![
                AppCommand::Session(
                    session_id.clone(),
                    SessionCommand::Stage(StageCommand::Start),
                ),
                AppCommand::Session(
                    session_id.clone(),
                    SessionCommand::Palette(PaletteCommand::Open),
                ),
                AppCommand::Session(
                    session_id.clone(),
                    SessionCommand::Input(InputCommand::ReplaceBuffer("hello seam".to_string())),
                ),
                AppCommand::Session(
                    session_id.clone(),
                    SessionCommand::Input(InputCommand::Submit),
                ),
                AppCommand::Session(
                    session_id.clone(),
                    SessionCommand::Tree(TreeCommand::MoveFocus { delta: 1 }),
                ),
                AppCommand::Session(
                    session_id.clone(),
                    SessionCommand::Modal(ModalCommand::Action(ModalAction::RetryStage(
                        StageId::Brainstorm,
                    ))),
                ),
                AppCommand::Global(GlobalCommand::Quit),
            ],
        };

        run_frontend(frontend).unwrap();

        let events = recorded_events.lock().unwrap();
        assert!(matches!(
            events.first().map(|event| &event.payload),
            Some(RootEventPayload::Snapshot(_))
        ));
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            RootEventPayload::SessionAdded(id, view)
                if id.as_ref() == "runtime-seam" && view.stage == Stage::IdeaInput
        )));
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            RootEventPayload::SessionChanged(id, SessionViewDelta::Stage(Stage::BrainstormRunning))
                if id.as_ref() == "runtime-seam"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            RootEventPayload::SessionChanged(id, SessionViewDelta::Palette(view))
                if id.as_ref() == "runtime-seam" && view.is_open
        )));
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            RootEventPayload::SessionChanged(id, SessionViewDelta::Chat(view))
                if id.as_ref() == "runtime-seam"
                    && view.messages.iter().any(|message| {
                        message.kind == ChatMessageKind::UserInput
                            && message.content.as_ref() == "hello seam"
                    })
        )));
        assert!(events.iter().any(|event| matches!(
            &event.payload,
            RootEventPayload::SessionChanged(id, SessionViewDelta::Tree(view))
                if id.as_ref() == "runtime-seam" && view.selected_index == Some(0)
        )));
    }
}
