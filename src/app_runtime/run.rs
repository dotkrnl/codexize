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
//! lands in later tasks — today's runtime still mutates `App` internal
//! state directly, and [`TerminalFrontend`] preserves that path so no
//! operator-visible behavior changes. The `seq`-before-publish ordering
//! invariant is encoded in [`publish`] and enforced as soon as the
//! runtime starts emitting granular deltas.
use super::frontend::{Frontend, FrontendConnector, ShutdownSignal, SnapshotHandle};
use super::root_view::{RootEvent, RootEventPayload, RootView};
use anyhow::Result;
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::mpsc;

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
    commands: mpsc::Receiver<super::AppCommand>,
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
    pub fn publish<F, E>(&self, mutate: F, event_for: E) -> Result<(), mpsc::SendError<RootEvent>>
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
        self.events.send(RootEvent {
            seq,
            payload: event_for(seq),
        })
    }

    /// Emit the initial `Snapshot` payload. Used by [`run_frontend`] before
    /// handing the connector to the frontend so a frontend can rely on
    /// receiving exactly one `Snapshot` before any granular delta.
    pub fn emit_initial_snapshot(&self) -> Result<(), mpsc::SendError<RootEvent>> {
        // The seeded `RootView` already has seq = 0; the event carries the
        // same seq so the spec's "match" invariant holds at startup too.
        let view = self.snapshot.read().clone();
        self.events.send(RootEvent {
            seq: view.seq,
            payload: RootEventPayload::Snapshot(view),
        })
    }

    pub fn shutdown(&self) -> ShutdownSignal {
        self.shutdown.clone()
    }
}

/// Build the frontend seam, emit the initial `Snapshot`, then hand off to
/// `frontend.run(connector)`.
///
/// `_publisher` is retained for the duration of the frontend's run so the
/// command receiver and snapshot writer stay alive even though no
/// background runtime loop is wired in this task yet. Without keeping it
/// alive, dropping the receiver would close the command channel and any
/// `commands.send(..)` from the frontend would fail spuriously.
pub fn run_frontend<F: Frontend>(frontend: F) -> Result<()> {
    let (connector, publisher) = build_connector();
    // Exactly one initial Snapshot before any granular delta (spec §
    // "Initialization flow"). A closed event channel here means no one is
    // listening yet, which is fine — the channel is consumed by the
    // frontend below.
    let _ = publisher.emit_initial_snapshot();
    let result = frontend.run(connector);
    drop(publisher);
    result
}

/// Thin shim that adapts today's `AppShell`-driven TUI loop onto the
/// [`Frontend`] trait. Milestone 4 replaces this with a real
/// `TerminalFrontend` that consumes `RootView` / `RootEvent` end-to-end.
///
/// `connector` is accepted but the inner TUI loop still drives state
/// directly via `AppShell::run_focused_terminal_app`; the connector is
/// kept alive (the initial Snapshot has been emitted, the command sender
/// stays open) so future incremental wiring can flip surfaces over one at
/// a time without touching the call site in `main.rs`.
pub struct TerminalFrontend<'a> {
    pub shell: &'a mut crate::app_shell::AppShell,
    pub terminal: &'a mut crate::ui::tui::AppTerminal,
}

impl<'a> Frontend for TerminalFrontend<'a> {
    fn run(self, _connector: FrontendConnector) -> Result<()> {
        self.shell.run_focused_terminal_app(self.terminal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
