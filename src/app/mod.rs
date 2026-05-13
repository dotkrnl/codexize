pub use crate::ui::chrome;
pub use crate::ui::clock;
pub(crate) use crate::ui::widgets::chat::state as chat_widget_view_model;
pub use crate::ui::widgets::chat::view as chat_widget;
mod builder_recovery;
mod events;
mod expansion;
mod finalization;
pub use crate::ui::focus_caps;
pub use crate::ui::footer;
pub(crate) use finalization::Reason;
pub(crate) mod guard;
mod lifecycle;
pub(crate) mod models;
pub use crate::ui::widgets::models_area::view as models_area;
mod notifications;
mod observation;
pub(crate) use crate::ui::config_panel;
pub(crate) use crate::ui::palette;
pub(crate) mod prior_attempts;
mod prompt_builders;
mod prompt_ctx;
mod prompt_support;
pub(crate) mod prompts;
mod retry_policy;
mod review_banner;
pub(crate) use crate::ui::render::state as render_view_model;
pub(crate) use crate::ui::render::view as render;
mod run_helpers;
mod stage_support;
pub use crate::ui::sheet;
pub use crate::ui::split;
mod state;
pub use crate::ui::status_line;
#[cfg(test)]
#[path = "tests_support.rs"]
pub(crate) mod test_support;
// The private app suites live in layer-owned directories, but remain declared
// here so they can exercise App internals without widening production APIs.
#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests_notifications;
#[cfg(test)]
#[path = "../app_runtime/tests/prompts/mod.rs"]
mod tests_prompts;
#[cfg(test)]
#[path = "../ui/tests/split_sync.rs"]
mod tests_split_sync;
pub(crate) use crate::ui::widgets::tree::view as tree;
pub(crate) mod watchdog;
pub mod yolo_exit;
pub(crate) use self::state::ModelRefreshState;
use self::tree::{NodeKey, VisibleNodeRow};
use crate::{
    cache,
    data::runner::supervise::session_supervisor::SessionSupervisor,
    selection::{CachedModel, QuotaError, SubscriptionKind},
    state::{Message, Node, SessionState},
};
pub(crate) use footer::keymap::{Capability, KeyBinding, render_keymap_line};
pub(crate) use sheet::bottom_sheet;
pub(crate) use status_line::{Severity, StatusLine};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant, SystemTime},
};
pub(crate) type RetryKey = (String, Option<u32>, u32);
pub(crate) type FailedModelSet = HashSet<(SubscriptionKind, String)>;
const DEFAULT_STAMP_TIMEOUT_MS: u64 = 1500;
const ENV_STAMP_TIMEOUT_MS: &str = "CODEXIZE_STAMP_TIMEOUT_MS";
const DEFAULT_EVENT_POLL_MS: u64 = 250;
const LIVE_SUMMARY_EVENT_POLL_MS: u64 = 50;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedPathState {
    pub exists: bool,
    pub modified_at: Option<SystemTime>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YoloExitSnapshot {
    pub live_summary: ObservedPathState,
    pub finish_stamp: ObservedPathState,
    pub stage_artifacts: Vec<ObservedPathState>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YoloExitObservation {
    pub snapshot: YoloExitSnapshot,
    pub saw_new_update: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpansionOverride {
    Expanded,
    Collapsed,
}
/// Identifies a running stage for Family B error modals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageId {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    Implementation,
    Review,
    FinalValidation,
    Dreaming,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalKind {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryLaunch {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    Recovery,
    RecoveryPlanReview,
    RecoverySharding,
    Coder,
    Reviewer,
    FinalValidation,
    Dreaming,
}
impl RetryLaunch {
    fn for_run(run: &crate::state::RunRecord) -> Option<Self> {
        // Recovery sub-stages all share `stage == "recovery"`, so we key off the
        // human-readable window label to preserve retry fidelity.
        if run.window_name.contains("[Recovery Plan Review]") {
            return Some(Self::RecoveryPlanReview);
        }
        if run.window_name.contains("[Recovery Sharding]") {
            return Some(Self::RecoverySharding);
        }
        match run.stage.as_str() {
            "brainstorm" => Some(Self::Brainstorm),
            "spec-review" => Some(Self::SpecReview),
            "planning" => Some(Self::Planning),
            "plan-review" => Some(Self::PlanReview),
            "sharding" => Some(Self::Sharding),
            "recovery" => Some(Self::Recovery),
            "coder" => Some(Self::Coder),
            "reviewer" => Some(Self::Reviewer),
            "final-validation" => Some(Self::FinalValidation),
            "dreaming" => Some(Self::Dreaming),
            _ => None,
        }
    }
}
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminationIntent {
    StopOnly,
    StopAndRetry(RetryLaunch),
    StopAndQuit,
    CancelSession,
}
impl TerminationIntent {
    fn summary(&self) -> &'static str {
        match self {
            Self::StopOnly => "stop without retry",
            Self::StopAndRetry(_) => "stop and retry",
            Self::StopAndQuit => "stop and quit",
            Self::CancelSession => "cancel session",
        }
    }
    fn in_progress_status(&self) -> &'static str {
        match self {
            Self::StopOnly => "Stopping agent...",
            Self::StopAndRetry(_) => "Stopping agent and queuing retry...",
            Self::StopAndQuit => "Stopping agent and quitting...",
            Self::CancelSession => "Cancelling session...",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingTermination {
    run_id: u64,
    intent: TerminationIntent,
}
impl PendingTermination {
    fn marker(&self) -> &'static str {
        match self.intent {
            TerminationIntent::StopOnly | TerminationIntent::StopAndQuit => "agent_stopped_by_user",
            TerminationIntent::StopAndRetry(_) => "agent_retry_requested_by_user",
            TerminationIntent::CancelSession => "session_cancel_requested_by_user",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandReturnTarget {
    Idea,
    FooterInteractive,
    SplitInteractive,
}
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct TestLaunchOutcome {
    pub(crate) exit_code: i32,
    pub(crate) artifact_contents: Option<String>,
    pub(crate) launch_error: Option<String>,
}
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct TestLaunchHarness {
    pub(crate) outcomes: std::collections::VecDeque<TestLaunchOutcome>,
}
pub const RESPONSIVE_HEIGHT_THRESHOLD: u16 = 60;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppStartupOrigin {
    #[default]
    Default,
    PickerCreated,
}
impl App {
    /// Return the `artifacts/spec.md` paths for every non-archived session that
    /// sorts earlier than the current session and is in `WaitingToImplement`.
    /// These represent the "expected future repository state" that brainstorm,
    /// spec review, and planning stages must consider.
    pub(crate) fn earlier_waiting_specs(&self) -> Vec<std::path::PathBuf> {
        let Ok(scanned) =
            crate::data::picker_io::scan_sessions_for_scheduler(&self.sessions_root())
        else {
            return Vec::new();
        };
        scanned
            .into_iter()
            .filter_map(|s| match s {
                crate::scheduler::ScannedSession::Loaded(session) => {
                    if session.session_id < self.supervisor.state.session_id
                        && session.current_phase == crate::state::Phase::WaitingToImplement
                    {
                        Some(
                            crate::state::session_dir(&session.session_id)
                                .join("artifacts/spec.md"),
                        )
                    } else {
                        None
                    }
                }
                crate::scheduler::ScannedSession::Corrupt { .. } => None,
            })
            .collect()
    }
}
pub struct App {
    pub(crate) supervisor: SessionSupervisor,
    pub(crate) nodes: Vec<Node>,
    pub(crate) visible_rows: Vec<VisibleNodeRow>,
    pub(crate) models: Vec<CachedModel>,
    pub(crate) model_refresh: ModelRefreshState,
    pub(crate) selected: usize,
    pub(crate) selected_key: Option<NodeKey>,
    pub(crate) collapsed_overrides: BTreeMap<NodeKey, ExpansionOverride>,
    pub(crate) viewport_top: usize,
    pub(crate) follow_tail: bool,
    /// When true, the viewport was intentionally paged away from the focused
    /// row and clamp_viewport should not pull it back toward focus.
    pub(crate) explicit_viewport_scroll: bool,
    /// While true, automatic progress events (startup, phase changes, run
    /// launches/retries, live-summary updates) move the focus arrow to the
    /// newest active run row. Manual focus moves and explicit viewport paging
    /// flip this off; the next phase transition or run launch flips it back on.
    pub(crate) progress_follow_active: bool,
    /// Snapshot of `messages.len()` taken when tail-follow was last
    /// disengaged. None while following. Used to count missed messages
    /// for the "v N new" badge.
    pub(crate) tail_detach_baseline: Option<usize>,
    pub(crate) body_inner_height: usize,
    pub(crate) body_inner_width: usize,
    pub(crate) split_target: Option<split::SplitTarget>,
    /// When true, the split transcript snaps to the latest visible tail on
    /// content/viewport changes. Manual split scrolling flips this off until
    /// the operator returns to the bottom of the transcript.
    pub(crate) split_follow_tail: bool,
    pub(crate) split_scroll_offset: usize,
    /// Cached from the last draw pass so lifecycle clamping can honor the
    /// full-body split mode used at small terminal heights.
    pub(crate) split_fullscreen: bool,
    pub(crate) input_mode: bool,
    pub(crate) input_buffer: String,
    pub(crate) input_cursor: usize,
    pub(crate) pending_view_path: Option<std::path::PathBuf>,
    pub(crate) confirm_back: bool,
    pub(crate) spinner_tick: usize,
    pub(crate) live_summary_spinner_visible: bool,
    pub(crate) pending_drain_deadline: Option<Instant>,
    pub(crate) pending_termination: Option<PendingTermination>,
    pub(crate) pending_quit_confirmation_run_id: Option<u64>,
    pub(crate) pending_cancel_confirmation: bool,
    pub(crate) interactive_exit_prompt_dismissed_at: Option<(u64, usize)>,
    pub(crate) pending_app_exit: bool,
    pub(crate) pending_shell_command: Option<String>,
    pub(crate) pending_yolo_toggle_gate: Option<&'static str>,
    #[cfg(test)]
    pub(crate) test_launch_harness: Option<std::sync::Arc<std::sync::Mutex<TestLaunchHarness>>>,
    pub(crate) status_line: Rc<RefCell<status_line::StatusLine>>,
    pub(crate) prev_models_mode: models_area::ModelsAreaMode,
    pub(crate) palette: palette::PaletteState,
    pub(crate) command_return_target: Option<CommandReturnTarget>,
    pub(crate) config_panel: Option<config_panel::ConfigPanelState>,
    /// Section name surfaced the last time the config panel closed, so
    /// reopening it within the same App restores that context. Reset to
    /// None across launches; the panel falls back to the default section
    /// when None or when the remembered section is no longer registered.
    pub(crate) last_config_section: Option<String>,
    /// Project name captured once at App construction (basename of the
    /// process cwd when the App was built). The top rule reads this
    /// instead of `std::env::current_dir()` so the title bar stays
    /// stable across the session — and so parallel tests that change
    /// the process cwd cannot leak tempdir names into the rendered
    /// title.
    pub(crate) ui_view: crate::data::config::view::UiView,
    pub(crate) project_name: String,
}
impl std::ops::Deref for App {
    type Target = SessionSupervisor;
    fn deref(&self) -> &Self::Target {
        &self.supervisor
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.supervisor
    }
}

fn default_expansion(
    row: &VisibleNodeRow,
    _current_node: usize,
    _active_keys: &BTreeSet<NodeKey>,
) -> bool {
    row.is_expandable()
}
fn effective_expansion(
    row: &VisibleNodeRow,
    current_node: usize,
    active_keys: &BTreeSet<NodeKey>,
    overrides: &BTreeMap<NodeKey, ExpansionOverride>,
) -> bool {
    if !row.is_expandable() {
        return false;
    }
    match overrides.get(&row.key) {
        Some(ExpansionOverride::Expanded) => true,
        Some(ExpansionOverride::Collapsed) => false,
        None => default_expansion(row, current_node, active_keys),
    }
}
fn startup_cache_has_expired_section(loaded: &cache::LoadedCache) -> bool {
    let dashboard_expired = loaded.dashboard.as_ref().is_none_or(|s| s.expired);
    let quotas_expired = loaded.quotas.as_ref().is_none_or(|s| s.expired);
    dashboard_expired || quotas_expired
}
