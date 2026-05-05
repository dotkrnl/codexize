//! Immutable, UI-neutral snapshot the runtime publishes for any UI to render.
//!
//! `AppView` and its sibling enums are the seam between [`crate::app_runtime`]
//! and [`crate::ui`]. They intentionally carry no `ratatui` or `crossterm`
//! types and no mutable cache handles — the runtime owns the underlying
//! state and a UI layer only reads via this view, then converts the values
//! into its own (ratatui, web, headless, …) presentation shape.
//!
//! The production TUI now consumes `AppView` for the seams that have been
//! migrated (top-rule mode badges read [`AppView::modes`], the terminal
//! loop publishes the view each tick) while a few legacy surfaces still
//! read state directly from [`crate::app::App`]. The view types remain
//! authoritative for the runtime/UI seam and are exercised by
//! [`crate::app_runtime::harness`].

use std::sync::Arc;

use crate::logic::pipeline::{Phase, RunRecord, RunStatus};

/// Severity tag for a UI-neutral status message. The TUI maps each variant
/// to a colour/style; a future web UI maps the same variants to its own
/// presentation primitives.
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

/// Pipeline stage identifier used by stage-scoped modals and commands.
///
/// Distinct from [`Phase`] because phases include intermediate states
/// (`*Paused`, `*Running`, recovery sub-phases) that the UI does not need
/// to disambiguate when surfacing a stage-error modal or routing a retry
/// command. Mirrors the `super::StageId` enum in the legacy `app` module
/// so that the migration can collapse them once the TUI consumes views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageId {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    Implementation,
    Review,
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
    InteractiveExitPrompt,
    SpecReviewPaused,
    PlanReviewPaused,
    StageError(StageId),
    FinalValidationBlocked,
}

/// Compact run summary for tree rows. The full [`RunRecord`] is also
/// available in [`AppView::agent_runs`]; this projection exists so a
/// future server-mode UI can render lists without re-deriving the
/// per-row label.
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
    /// Active pipeline phase as derived by [`crate::logic`].
    pub phase: Phase,
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
            phase: Phase::IdeaInput,
            modal: None,
            status: None,
            agent_runs: Arc::from(Vec::<AgentRunSummary>::new()),
            follow_tail: true,
            agent_running: false,
            modes: ModeFlags::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::EffortLevel;
    use crate::logic::pipeline::LaunchModes;
    use chrono::Utc;

    fn sample_run(id: u64, stage: &str, status: RunStatus) -> RunRecord {
        RunRecord {
            id,
            stage: stage.to_string(),
            task_id: None,
            round: 0,
            attempt: 0,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: format!("codexize-run-{id}-{stage}"),
            started_at: Utc::now(),
            ended_at: None,
            status,
            error: None,
            effort: EffortLevel::Normal,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    #[test]
    fn empty_view_has_no_modal_or_status() {
        let view = AppView::empty("test-session");
        assert_eq!(view.session_id.as_ref(), "test-session");
        assert!(view.modal.is_none());
        assert!(view.status.is_none());
        assert!(view.agent_runs.is_empty());
        assert!(view.follow_tail);
        assert!(!view.agent_running);
        assert_eq!(view.phase, Phase::IdeaInput);
        assert_eq!(view.modes, ModeFlags::default());
    }

    #[test]
    fn agent_run_summary_projects_record_fields() {
        let run = sample_run(42, "brainstorm", RunStatus::Running);
        let summary = AgentRunSummary::from_record(&run);
        assert_eq!(summary.id, 42);
        assert_eq!(summary.stage.as_ref(), "brainstorm");
        assert_eq!(summary.window_name.as_ref(), "codexize-run-42-brainstorm");
        assert_eq!(summary.status, RunStatus::Running);
    }
}
