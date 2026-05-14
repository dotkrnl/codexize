//! Immutable, UI-neutral snapshot the runtime publishes for any UI to render.
//!
//! `AppView` and its sibling enums are the seam between [`crate::app_runtime`]
//! and [`crate::ui`]. They intentionally carry no `ratatui` or `crossterm`
//! types and no mutable cache handles — the runtime owns the underlying
//! state and a UI layer only reads via this view, then converts the values
//! into its own (ratatui, web, headless, …) presentation shape.
//!
//! The production TUI consumes `AppView` for top-rule mode badges and the
//! terminal loop publishes the view each tick. Some focus-local surfaces still
//! read state directly from [`crate::app::App`]. The view types remain
//! authoritative for the runtime/UI seam.
use crate::logic::pipeline::Stage;
use crate::state::{RunRecord, RunStatus};
use std::sync::Arc;
/// Severity tag for a UI-neutral status message. Each UI maps the variants
/// to its own presentation primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSeverity {
    Info,
    Warn,
    Error,
}
/// Single line of operator-facing status text. Owned and immutable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    pub text: Arc<str>,
    pub severity: StatusSeverity,
}
/// Operator-visible stage-error target used by stage-scoped modals and
/// retry commands.
///
/// Distinct from [`Stage`] because stages mix modal state, pipeline position,
/// and running-agent identity. This enum names the stage the operator sees
/// in a stage-error modal, so retry can relaunch the exact lifecycle stage
/// that failed even when several stages share one lifecycle stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum StageId {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    RepoStateUpdate,
    Sharding,
    Implementation,
    Recovery,
    RecoveryPlanReview,
    RecoverySharding,
    Review,
    Simplification,
    FinalValidation,
    Dreaming,
}
/// Modal kinds the runtime asks the UI to render. The UI decides the
/// rendering, but cannot invent modals — only the runtime can transition
/// in/out of these states because they are rooted in pipeline + guard
/// decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    SkipToImpl,
    GitGuard,
    QuitRunningAgent,
    CancelSession,
    InteractiveExitPrompt,
    SpecReviewPaused,
    PlanReviewPaused,
    StageError(StageId),
    FinalValidationBlocked,
    DreamingDecision,
}
/// Compact run summary for tree rows. The full [`RunRecord`] is also
/// available in [`AppView::agent_runs`]; this projection lets UI callers
/// render lists without re-deriving the per-row label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunSummary {
    pub id: u64,
    pub stage: Arc<str>,
    pub window_name: Arc<str>,
    pub status: RunStatus,
}
impl AgentRunSummary {
    pub fn from_record(run: &RunRecord) -> Self {
        Self {
            id: run.id,
            stage: Arc::from(run.stage.as_str()),
            window_name: Arc::from(run.window_name.as_str()),
            status: run.status,
        }
    }
}
/// UI-neutral mirror of the operator-toggleable mode flags. Mirrors
/// [`crate::logic::pipeline::Modes`] without dragging the persistence-shaped
/// type across the seam.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModeFlags {
    pub yolo: bool,
    pub cheap: bool,
}
/// Immutable, UI-neutral derived snapshot for any UI to render.
///
/// The view is built from the runtime's authoritative state and shipped
/// to the UI layer over a channel. UIs convert the values into their own
/// presentation primitives (ratatui `Line`/`Span`, HTML, JSON, …) but
/// must not mutate the source state directly — they emit
/// [`crate::app_runtime::AppCommand`] back instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppView {
    /// Session identifier (`.codexize/runs/<session_id>/`).
    pub session_id: Arc<str>,
    /// Active pipeline stage as derived by [`crate::logic`].
    pub stage: Stage,
    /// Active modal, if any. The UI overlays the corresponding prompt.
    pub modal: Option<ModalKind>,
    /// Latest status-line entry, if any.
    pub status: Option<StatusMessage>,
    /// Snapshot of agent runs known to the runtime. Owned so the UI may
    /// keep the slice across re-renders without back-pressure on the
    /// runtime's authoritative copy.
    pub agent_runs: Arc<[AgentRunSummary]>,
    /// True when the UI should auto-scroll to the newest tail content.
    pub follow_tail: bool,
    /// True when an agent run is currently in flight; the UI uses this
    /// to gate inputs (e.g. quit confirmation, palette commands).
    pub agent_running: bool,
    /// Operator mode flags (YOLO / Cheap). Drives the top-rule mode badges
    /// and any UI surface that conditions on launch policy.
    pub modes: ModeFlags,
}
impl AppView {
    /// Empty view used by the harness and as a starting point before the
    /// runtime publishes its first real snapshot.
    pub fn empty(session_id: impl Into<Arc<str>>) -> Self {
        Self {
            session_id: session_id.into(),
            stage: Stage::IdeaInput,
            modal: None,
            status: None,
            agent_runs: Arc::from(Vec::<AgentRunSummary>::new()),
            follow_tail: true,
            agent_running: false,
            modes: ModeFlags::default(),
        }
    }
}
