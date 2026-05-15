//! Aggregate snapshot/event types pinned by the spec.
//!
//! `RootView` is the top-level UI-neutral projection of the runtime: every
//! frontend reads from it (via [`super::frontend::SnapshotHandle`]) or
//! subscribes to its typed delta stream ([`RootEvent`]). The runtime
//! guarantees that whenever a frontend observes `RootEvent { seq: N, .. }`,
//! the next `snapshot.read()` has `RootView::seq >= N` — the publish
//! strictly follows the write under the snapshot lock.
//!
//! This task lands the spec-pinned shapes with deliberately minimal
//! sub-view stubs. Later tasks extract per-surface fields into
//! `app_runtime/views/` and enrich `ShellView` / `SessionView` with the
//! real surfaces (tree, chat, palette, …) the TUI renders today.
use std::collections::BTreeMap;
use std::sync::Arc;

/// Stable, UI-neutral identifier for a session. Mirrors the on-disk
/// `.codexize/runs/<session_id>/` directory name. `Arc<str>` keeps clones
/// cheap when the same identifier appears across `RootView::sessions`,
/// `focus`, and event payloads.
pub type SessionId = Arc<str>;

/// Aggregate snapshot of everything the runtime currently exposes to a
/// frontend. Carries a monotonically increasing `seq` so frontends can
/// reconcile against the [`RootEvent`] stream.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct RootView {
    /// Monotonic snapshot sequence number. Each runtime write bumps it by
    /// one before the matching [`RootEvent`] is published.
    pub seq: u64,
    /// Shell-level (cross-session) state — sidebar rows, focus area, etc.
    pub shell: ShellView,
    /// Per-session sub-views keyed by [`SessionId`]. `BTreeMap` keeps the
    /// iteration order stable across snapshots for deterministic rendering.
    pub sessions: BTreeMap<SessionId, SessionView>,
    /// Currently focused session. Always present in `sessions` once the
    /// runtime has published a real snapshot; for the seeded empty
    /// `RootView` (before any session exists) the value is the empty
    /// string and `sessions` is empty.
    pub focus: SessionId,
}

impl RootView {
    /// Seed `RootView` with empty shell/session state and `seq = 0`.
    pub fn initial() -> Self {
        Self {
            seq: 0,
            shell: ShellView::default(),
            sessions: BTreeMap::new(),
            focus: Arc::<str>::from(""),
        }
    }
}

/// One delta in the typed change stream. The `seq` matches the
/// [`RootView::seq`] the runtime wrote immediately before publishing this
/// event.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RootEvent {
    pub seq: u64,
    pub payload: RootEventPayload,
}

/// Typed payload for a [`RootEvent`]. Granular variants let frontends
/// subscribe to the surfaces they care about; `Snapshot` is the
/// initialization event emitted exactly once per frontend connection
/// before any granular delta.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum RootEventPayload {
    /// Cross-session shell state changed.
    ShellChanged(ShellViewDelta),
    /// A new session appeared. Carries the full `SessionView` so the
    /// frontend does not need to chase a follow-up snapshot.
    SessionAdded(SessionId, SessionView),
    /// A session was torn down.
    SessionRemoved(SessionId),
    /// A field on an existing session's `SessionView` changed.
    SessionChanged(SessionId, SessionViewDelta),
    /// The focused session changed.
    FocusChanged(SessionId),
    /// Initial complete snapshot. Emitted exactly once per frontend
    /// connection before any granular delta.
    Snapshot(RootView),
    /// Machine-readable error (e.g. stdin JSON parse failure in the
    /// headless frontend). The text is intentionally free-form; frontends
    /// log or surface it as appropriate.
    Error(String),
}

/// Stub shell view. Later tasks migrate sidebar rows, shell focus, and
/// related projections into per-surface files under `app_runtime/views/`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ShellView {}

/// Placeholder for the sidebar row projection. The actual fields land in
/// `app_runtime/views/shell.rs` in a later task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SidebarRow {}

/// Placeholder for the shell focus enum. The actual variants land in
/// `app_runtime/views/shell.rs` in a later task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ShellFocus {
    /// Shell focus stub — replaced with the real variants later.
    Placeholder,
}

/// Stub session view. Later tasks attach per-surface sub-views (tree,
/// chat, palette, …) plus the spec-pinned scalar fields (`stage`,
/// `modes`, `status`, `agent_runs`, `modal`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SessionView {}

/// Granular shell-level delta. Variants are stubs today; the real set is
/// extended as `ShellView` grows real fields.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ShellViewDelta {
    /// Whole-shell replacement, used on recovery or first emit.
    Full(ShellView),
    /// Sidebar rows replaced (cheap because of the `Arc<[…]>` payload).
    SidebarRows(Arc<[SidebarRow]>),
    /// Shell focus changed.
    ShellFocus(ShellFocus),
}

/// Granular session-level delta. The spec guarantees one variant per
/// mutable field of `SessionView`; today's stub `SessionView` has no
/// fields so only `Full` is meaningful. Later tasks add one variant per
/// extracted sub-view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum SessionViewDelta {
    /// Whole-session replacement, used on `SessionAdded` or recovery.
    Full(SessionView),
}
