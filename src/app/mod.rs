pub mod chat_widget;
mod chat_widget_view_model;
pub mod chrome;
mod clock;
mod events;
mod expansion;
mod finalization;
mod focus_caps;
mod footer;
mod guard;
mod launch;
mod lifecycle;
mod models;
mod models_area;
mod models_area_view_model;
mod observation;
pub(crate) mod palette;
mod prompts;
mod render;
mod render_view_model;
mod sheet;
mod split;
mod state;
mod status_line;
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
mod tree;
mod tree_view_model;
mod yolo_exit;

pub(crate) use footer::keymap::{Capability, KeyBinding, render_keymap_line};
pub(crate) use sheet::bottom_sheet;
pub(crate) use status_line::{Severity, StatusLine};

use crate::{
    cache,
    selection::{CachedModel, QuotaError, VendorKind, ranking::VersionIndex},
    state::{Message, Node, SessionState},
};

use self::{
    state::ModelRefreshState,
    tree::{NodeKey, VisibleNodeRow},
};

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

type RetryKey = (String, Option<u32>, u32);
type FailedModelSet = HashSet<(VendorKind, String)>;
const DEFAULT_STAMP_TIMEOUT_MS: u64 = 1500;
const ENV_STAMP_TIMEOUT_MS: &str = "CODEXIZE_STAMP_TIMEOUT_MS";
const DEFAULT_EVENT_POLL_MS: u64 = 250;
const LIVE_SUMMARY_EVENT_POLL_MS: u64 = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedPathState {
    exists: bool,
    modified_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct YoloExitSnapshot {
    live_summary: ObservedPathState,
    finish_stamp: ObservedPathState,
    stage_artifacts: Vec<ObservedPathState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct YoloExitObservation {
    snapshot: YoloExitSnapshot,
    saw_new_update: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpansionOverride {
    Expanded,
    Collapsed,
}

/// Identifies a running stage for Family B error modals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StageId {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    Implementation,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModalKind {
    SkipToImpl,
    GitGuard,
    QuitRunningAgent,
    SpecReviewPaused,
    PlanReviewPaused,
    StageError(StageId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetryLaunch {
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
enum TerminationIntent {
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
struct PendingTermination {
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
pub(super) enum CommandReturnTarget {
    Idea,
    FooterInteractive,
    SplitInteractive,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestLaunchOutcome {
    exit_code: i32,
    artifact_contents: Option<String>,
    launch_error: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestLaunchHarness {
    outcomes: std::collections::VecDeque<TestLaunchOutcome>,
}

pub const RESPONSIVE_HEIGHT_THRESHOLD: u16 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppStartupOrigin {
    #[default]
    Default,
    PickerCreated,
}

pub struct App {
    state: SessionState,
    nodes: Vec<Node>,
    visible_rows: Vec<VisibleNodeRow>,
    models: Vec<CachedModel>,
    versions: VersionIndex,
    model_refresh: ModelRefreshState,
    selected: usize,
    selected_key: Option<NodeKey>,
    collapsed_overrides: BTreeMap<NodeKey, ExpansionOverride>,
    viewport_top: usize,
    follow_tail: bool,
    /// When true, the viewport was intentionally paged away from the focused
    /// row and clamp_viewport should not pull it back toward focus.
    explicit_viewport_scroll: bool,
    /// While true, automatic progress events (startup, phase changes, run
    /// launches/retries, live-summary updates) move the focus arrow to the
    /// newest active run row. Manual focus moves and explicit viewport paging
    /// flip this off; the next phase transition or run launch flips it back on.
    progress_follow_active: bool,
    /// Snapshot of `messages.len()` taken when tail-follow was last
    /// disengaged. None while following. Used to count missed messages
    /// for the "v N new" badge.
    tail_detach_baseline: Option<usize>,
    body_inner_height: usize,
    body_inner_width: usize,
    split_target: Option<split::SplitTarget>,
    /// When true, the split transcript snaps to the latest visible tail on
    /// content/viewport changes. Manual split scrolling flips this off until
    /// the operator returns to the bottom of the transcript.
    split_follow_tail: bool,
    split_scroll_offset: usize,
    /// Cached from the last draw pass so lifecycle clamping can honor the
    /// full-body split mode used at small terminal heights.
    split_fullscreen: bool,
    input_mode: bool,
    input_buffer: String,
    input_cursor: usize,
    pending_view_path: Option<std::path::PathBuf>,
    confirm_back: bool,
    startup_origin: AppStartupOrigin,
    run_launched: bool,
    quota_errors: Vec<QuotaError>,
    quota_retry_delay: Duration,
    agent_line_count: usize,
    agent_content_hash: u64,
    agent_last_change: Option<Instant>,
    spinner_tick: usize,
    live_summary_spinner_visible: bool,
    live_summary_watcher: Option<notify::RecommendedWatcher>,
    live_summary_change_rx: Option<mpsc::Receiver<()>>,
    live_summary_path: Option<std::path::PathBuf>,
    live_summary_cached_text: String,
    live_summary_cached_mtime: Option<std::time::SystemTime>,
    pending_drain_deadline: Option<Instant>,
    pending_termination: Option<PendingTermination>,
    pending_quit_confirmation_run_id: Option<u64>,
    pending_app_exit: bool,
    current_run_id: Option<u64>,
    failed_models: HashMap<RetryKey, FailedModelSet>,
    pending_yolo_toggle_gate: Option<&'static str>,
    yolo_exit_issued: HashSet<u64>,
    yolo_exit_observations: HashMap<u64, YoloExitObservation>,
    #[cfg(test)]
    test_launch_harness: Option<std::sync::Arc<std::sync::Mutex<TestLaunchHarness>>>,
    messages: Vec<Message>,
    status_line: Rc<RefCell<status_line::StatusLine>>,
    prev_models_mode: models_area::ModelsAreaMode,
    palette: palette::PaletteState,
    command_return_target: Option<CommandReturnTarget>,
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
