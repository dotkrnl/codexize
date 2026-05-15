//! Frontend seam: trait, connector, snapshot handle, and shutdown signal.
//!
//! Every frontend (today's TUI, the planned `HeadlessFrontend`, and any
//! `#[cfg(test)]` recording double) implements [`Frontend`] and receives a
//! single [`FrontendConnector`] when it is spawned. The connector exposes
//! only what a frontend is allowed to touch: a pull-based snapshot of the
//! current [`RootView`], the typed event stream, a sender for operator-intent
//! [`AppCommand`]s, and a cooperative shutdown flag.
//!
//! Sync shapes are intentional — both planned frontend implementations are
//! sync (the TUI does blocking crossterm reads; `HeadlessFrontend` does
//! blocking stdin reads). See `spec.md` §"Frontend trait".
use super::AppCommand;
use super::root_view::{RootEvent, RootView};
use parking_lot::RwLock;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
#[cfg(test)]
use std::time::Duration;

/// Implemented by any concrete frontend (TUI, headless, recording double).
///
/// A frontend takes ownership of itself and the connector and drives its
/// own loop. It is responsible for multiplexing input (e.g. terminal
/// events, stdin lines) with the [`FrontendConnector::events`] receiver so
/// neither side starves the other.
pub trait Frontend {
    fn run(self, connector: FrontendConnector) -> anyhow::Result<()>;
}

/// Channels and handles a [`Frontend`] is allowed to touch.
pub struct FrontendConnector {
    /// Pull-based handle returning the current [`RootView`] by cheap clone.
    pub snapshot: SnapshotHandle,
    /// Typed event stream. The runtime emits exactly one
    /// [`super::root_view::RootEventPayload::Snapshot`] at startup, then
    /// granular deltas as state changes.
    pub events: mpsc::Receiver<RootEvent>,
    /// Sender for operator-intent commands. The only way the frontend can
    /// influence runtime state.
    pub commands: mpsc::Sender<AppCommand>,
    /// Cooperative shutdown flag. Frontends poll this between iterations.
    pub shutdown: ShutdownSignal,
}

/// Pull-based snapshot of the current [`RootView`].
///
/// Internally an `Arc<RwLock<RootView>>` so multiple frontends (and the
/// runtime's writer) may share it. `read()` returns a fresh clone so the
/// caller never holds the read lock across its own logic.
#[derive(Clone)]
pub struct SnapshotHandle {
    inner: Arc<RwLock<RootView>>,
}

impl SnapshotHandle {
    pub fn new(inner: Arc<RwLock<RootView>>) -> Self {
        Self { inner }
    }

    /// Return a cheap clone of the current [`RootView`].
    ///
    /// The clone is cheap because heavy sub-view fields are held behind
    /// `Arc<…>` once those sub-views grow real content; for now `RootView`
    /// is small enough that the clone is trivial.
    pub fn read(&self) -> RootView {
        self.inner.read().clone()
    }
}

/// Cooperative shutdown signal shared between the runtime and any frontend.
///
/// Set once by whichever side decides to exit (Ctrl-C handler, runtime
/// quit, frontend error); polled by frontends between iterations.
#[derive(Clone, Default)]
pub struct ShutdownSignal {
    token: Arc<AtomicBool>,
}

impl ShutdownSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark shutdown as requested. Idempotent.
    pub fn set(&self) {
        self.token.store(true, Ordering::SeqCst);
    }

    /// True once `set` has been called.
    pub fn is_set(&self) -> bool {
        self.token.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
pub struct RecordingFrontend {
    pub recorded_events: Arc<Mutex<Vec<RootEvent>>>,
    pub scripted_commands: Vec<AppCommand>,
}

#[cfg(test)]
impl Frontend for RecordingFrontend {
    fn run(self, connector: FrontendConnector) -> anyhow::Result<()> {
        let FrontendConnector {
            events,
            commands,
            shutdown,
            ..
        } = connector;
        let mut scripted = self.scripted_commands.into_iter();

        loop {
            while let Ok(event) = events.try_recv() {
                self.recorded_events.lock().unwrap().push(event);
            }

            if shutdown.is_set() {
                break;
            }

            let Some(command) = scripted.next() else {
                while let Ok(event) = events.try_recv() {
                    self.recorded_events.lock().unwrap().push(event);
                }
                while let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
                    self.recorded_events.lock().unwrap().push(event);
                }
                break;
            };

            commands.send(command).map_err(|err| {
                anyhow::anyhow!("recording frontend command channel closed: {err}")
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_runtime::commands::GlobalCommand;
    use crate::app_runtime::root_view::{RootEventPayload, RootView};

    #[test]
    fn recording_frontend_drains_events_and_sends_scripted_commands() {
        let snapshot = Arc::new(RwLock::new(RootView::initial()));
        let snapshot_handle = SnapshotHandle::new(Arc::clone(&snapshot));
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let shutdown = ShutdownSignal::new();
        event_tx
            .send(RootEvent {
                seq: 0,
                payload: RootEventPayload::Snapshot(snapshot.read().clone()),
            })
            .unwrap();
        {
            let mut view = snapshot.write();
            view.focus = Arc::<str>::from("recorded-session");
            view.seq = 1;
        }
        event_tx
            .send(RootEvent {
                seq: 1,
                payload: RootEventPayload::FocusChanged(Arc::<str>::from("recorded-session")),
            })
            .unwrap();

        let recorded_events = Arc::new(Mutex::new(Vec::new()));
        let frontend = RecordingFrontend {
            recorded_events: Arc::clone(&recorded_events),
            scripted_commands: vec![AppCommand::Global(GlobalCommand::Quit)],
        };
        let connector = FrontendConnector {
            snapshot: snapshot_handle.clone(),
            events: event_rx,
            commands: command_tx,
            shutdown,
        };

        frontend.run(connector).unwrap();

        let events = recorded_events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].payload, RootEventPayload::Snapshot(_)));
        assert_eq!(events[1].seq, 1);
        assert!(matches!(
            events[1].payload,
            RootEventPayload::FocusChanged(_)
        ));
        assert!(snapshot_handle.read().seq >= events[1].seq);

        let command = command_rx.try_recv().expect("scripted command");
        assert_eq!(command, AppCommand::Global(GlobalCommand::Quit));
    }
}
