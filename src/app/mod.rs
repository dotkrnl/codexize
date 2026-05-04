pub use crate::ui::chrome;
pub use crate::ui::clock;
pub(crate) use crate::ui::widgets::chat::state as chat_widget_view_model;
pub use crate::ui::widgets::chat::view as chat_widget;
mod events;
mod expansion;
mod finalization;
pub use crate::ui::focus_caps;
pub use crate::ui::footer;
pub(crate) mod guard;
mod lifecycle;
pub(crate) mod models;
pub use crate::ui::widgets::models_area::view as models_area;
mod observation;
pub(crate) use crate::ui::palette;
mod prompt_render;
pub(crate) mod prompts;
mod review_banner;
pub(crate) use crate::ui::render::state as render_view_model;
pub(crate) use crate::ui::render::view as render;
pub use crate::ui::sheet;
pub use crate::ui::split;
mod state;
pub use crate::ui::status_line;
#[cfg(test)]
mod test_harness;
#[cfg(test)]
mod tests_finalization;
#[cfg(test)]
mod tests_launch;
#[cfg(test)]
mod tests_lifecycle;
#[cfg(test)]
mod tests_prompts;
#[cfg(test)]
mod tests_split_sync;
#[cfg(test)]
mod tests_yolo;
pub(crate) use crate::ui::widgets::tree::view as tree;
pub(crate) mod watchdog;
mod yolo_exit;

pub(crate) use footer::keymap::{Capability, KeyBinding, render_keymap_line};
pub(crate) use sheet::bottom_sheet;
pub(crate) use status_line::{Severity, StatusLine};

use crate::{
    cache,
    selection::{CachedModel, QuotaError, VendorKind, ranking::VersionIndex},
    state::{Message, Node, SessionState},
};

pub(crate) use self::state::ModelRefreshState;
use self::tree::{NodeKey, VisibleNodeRow};

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant, SystemTime},
};

pub(crate) type RetryKey = (String, Option<u32>, u32);
pub(crate) type FailedModelSet = HashSet<(VendorKind, String)>;
const DEFAULT_STAMP_TIMEOUT_MS: u64 = 1500;
const ENV_STAMP_TIMEOUT_MS: &str = "CODEXIZE_STAMP_TIMEOUT_MS";
const DEFAULT_EVENT_POLL_MS: u64 = 250;
const LIVE_SUMMARY_EVENT_POLL_MS: u64 = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedPathState {
    exists: bool,
    modified_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct YoloExitSnapshot {
    live_summary: ObservedPathState,
    finish_stamp: ObservedPathState,
    stage_artifacts: Vec<ObservedPathState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct YoloExitObservation {
    snapshot: YoloExitSnapshot,
    saw_new_update: bool,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalKind {
    SkipToImpl,
    GitGuard,
    QuitRunningAgent,
    InteractiveExitPrompt,
    SpecReviewPaused,
    PlanReviewPaused,
    StageError(StageId),
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
}

impl TerminationIntent {
    fn summary(&self) -> &'static str {
        match self {
            Self::StopOnly => "stop without retry",
            Self::StopAndRetry(_) => "stop and retry",
            Self::StopAndQuit => "stop and quit",
        }
    }

    fn in_progress_status(&self) -> &'static str {
        match self {
            Self::StopOnly => "Stopping agent...",
            Self::StopAndRetry(_) => "Stopping agent and queuing retry...",
            Self::StopAndQuit => "Stopping agent and quitting...",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingTermination {
    run_id: u64,
    intent: TerminationIntent,
}

impl PendingTermination {
    #[cfg(test)]
    fn new_stop_only(run_id: u64) -> Self {
        Self {
            run_id,
            intent: TerminationIntent::StopOnly,
        }
    }

    fn marker(&self) -> &'static str {
        match self.intent {
            TerminationIntent::StopOnly | TerminationIntent::StopAndQuit => "agent_stopped_by_user",
            TerminationIntent::StopAndRetry(_) => "agent_retry_requested_by_user",
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

pub struct App {
    pub(crate) state: SessionState,
    pub(crate) nodes: Vec<Node>,
    pub(crate) visible_rows: Vec<VisibleNodeRow>,
    pub(crate) models: Vec<CachedModel>,
    pub(crate) versions: VersionIndex,
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
    pub(crate) startup_origin: AppStartupOrigin,
    pub(crate) run_launched: bool,
    pub(crate) quota_errors: Vec<QuotaError>,
    pub(crate) quota_retry_delay: Duration,
    pub(crate) agent_line_count: usize,
    pub(crate) agent_content_hash: u64,
    pub(crate) agent_last_change: Option<Instant>,
    pub(crate) spinner_tick: usize,
    pub(crate) live_summary_spinner_visible: bool,
    pub(crate) live_summary_watcher: Option<notify::RecommendedWatcher>,
    pub(crate) live_summary_change_events: Option<crate::data::events::LiveSummaryEvents>,
    pub(crate) live_summary_path: Option<std::path::PathBuf>,
    pub(crate) live_summary_cached_text: String,
    pub(crate) live_summary_cached_mtime: Option<std::time::SystemTime>,
    pub(crate) pending_drain_deadline: Option<Instant>,
    pub(crate) pending_termination: Option<PendingTermination>,
    pub(crate) pending_quit_confirmation_run_id: Option<u64>,
    pub(crate) interactive_exit_prompt_dismissed_at: Option<(u64, usize)>,
    pub(crate) pending_app_exit: bool,
    pub(crate) current_run_id: Option<u64>,
    pub(crate) failed_models: HashMap<RetryKey, FailedModelSet>,
    pub(crate) pending_yolo_toggle_gate: Option<&'static str>,
    pub(crate) yolo_exit_issued: HashSet<u64>,
    pub(crate) yolo_exit_observations: HashMap<u64, YoloExitObservation>,
    /// Per-run liveness watchdog state. Allocated as part of task 1
    /// scaffolding; the App-side lifecycle hookup that inserts/removes
    /// entries (and ticks `evaluate`) lands with task 2.
    pub(crate) watchdog: watchdog::WatchdogRegistry,
    #[cfg(test)]
    pub(crate) test_launch_harness: Option<std::sync::Arc<std::sync::Mutex<TestLaunchHarness>>>,
    pub(crate) messages: Vec<Message>,
    pub(crate) status_line: Rc<RefCell<status_line::StatusLine>>,
    pub(crate) prev_models_mode: models_area::ModelsAreaMode,
    pub(crate) palette: palette::PaletteState,
    pub(crate) command_return_target: Option<CommandReturnTarget>,
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
    let dashboard_expired = loaded.dashboard.as_ref().map(|s| s.expired).unwrap_or(true);
    let quotas_expired = loaded.quotas.as_ref().map(|s| s.expired).unwrap_or(true);
    dashboard_expired || quotas_expired
}

#[doc(hidden)]
pub mod snapshot_support {
    use super::*;

    pub fn default_footer_keymap(width: u16) -> String {
        let line = footer::keymap::keymap(
            crate::state::Phase::IdeaInput,
            None,
            focus_caps::FocusCaps {
                can_expand: false,
                can_edit: false,
                can_back: false,
                can_input: false,
                can_split: false,
            },
            false,
            false,
            width,
        )
        .to_string()
        .trim_end()
        .to_string();
        format!("{line}\n")
    }

    pub fn warn_status_line() -> String {
        let mut line = status_line::StatusLine::new();
        line.push(
            "warn: smoke snapshot".to_string(),
            status_line::Severity::Warn,
            Duration::from_secs(10),
        );
        let line = line
            .render()
            .map(|line| line.to_string())
            .unwrap_or_default();
        format!("{line}\n")
    }
}
