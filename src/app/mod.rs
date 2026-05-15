mod builder_recovery;
mod chat;
pub(crate) mod config_panel;
mod events;
mod expansion;
mod finalization;
pub(crate) mod keys;
pub(crate) use finalization::Reason;
mod frame_cache;
pub(crate) mod guard;
pub(crate) mod input_editor;
mod lifecycle;
pub(crate) mod models;
mod notifications;
mod observation;
pub(crate) mod palette;
pub(crate) mod prior_attempts;
mod prompt_builders;
mod prompt_ctx;
pub(crate) mod prompts;
pub(crate) mod render_helpers;
mod retry_policy;
mod review_banner;
mod run_helpers;
mod split;
mod stage_support;
mod state;
pub(crate) mod status_line;
#[cfg(test)]
#[path = "tests_support.rs"]
pub(crate) mod test_support;
// The private app suites live in layer-owned directories, but remain declared
// here so they can exercise App internals without widening production APIs.
#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests_notifications;
#[cfg(test)]
#[path = "split_sync_tests.rs"]
mod tests_split_sync;
pub(crate) mod watchdog;
mod yolo_exit;
pub(crate) use self::state::ModelRefreshState;
pub(crate) mod tree;
use self::models::ModelsAreaMode;
use self::status_line::Severity;
use self::tree::{NodeKey, VisibleNodeRow};
use crate::app_runtime::views::split::SplitTargetView as SplitTarget;
use crate::data::cache;
use crate::selection::{CachedModel, QuotaError, SubscriptionKind};
use crate::state::{Message, Node, SessionState};
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
/// Operator-visible stage-error target for modals and retry commands.
///
/// This is distinct from `crate::lifecycle::StageId`: it names the stage
/// surface the operator interacts with, while lifecycle stages remain the
/// scheduler's internal dispatch keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageId {
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
/// Side effects parked on the App when an operator rewind lands while an
/// agent is still alive. The runner is asked to cancel synchronously; the
/// FSM stays in `Stopping` carrying [`crate::lifecycle::AfterStop::Rewind`]
/// until `finalize_run_record` confirms the agent is dead. At that point
/// [`App::apply_after_stop_rewind`] consumes this slot.
///
/// This slot lives in-memory only; a TUI crash mid-rewind falls back to
/// the resume path's repair logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingRewindApply {
    pub(crate) target: crate::lifecycle::Stage,
    pub(crate) spec: Option<crate::lifecycle::StageSpec>,
    pub(crate) cleanup: crate::lifecycle::CleanupPlan,
    pub(crate) clear_pending: bool,
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
    /// Persist session state and log failures instead of silently dropping them.
    ///
    /// Mirrors the App-side lifecycle-overlay fields (`paused_at_stage`,
    /// `pending_decisions`) into `state` immediately before the write so the
    /// on-disk `session.toml` reflects the current FSM context. The reverse
    /// copy happens in `App::new` from the loaded `SessionState` into the
    /// App mirrors.
    pub(crate) fn save_state(&mut self) {
        self.state.paused_at_stage = self.paused_at_stage;
        self.state.pending_decisions = self.pending_decisions.clone();
        if let Err(e) = self.state.save() {
            tracing::warn!("failed to save session state: {e}");
        }
    }

    /// Guard that the model list has been loaded. If empty, records an agent
    /// error, saves state, rebuilds the tree, and returns `false`.
    pub(crate) fn guard_models_loaded(&mut self) -> bool {
        if self.models.is_empty() {
            self.record_agent_error(
                "model list not yet loaded — wait a moment and try again".to_string(),
            );
            self.save_state();
            self.rebuild_tree_view(None);
            return false;
        }
        true
    }

    /// Return the `artifacts/spec.md` paths for every non-archived session that
    /// sorts earlier than the current session and is in `WaitingToImplement`.
    /// These represent the expected future repository state that brainstorm,
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
                    if session.session_id < self.state.session_id
                        && session.current_stage == crate::state::Stage::WaitingToImplement
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
    pub(crate) state: SessionState,
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
    /// While true, automatic progress events (startup, stage changes, run
    /// launches/retries, live-summary updates) move the focus arrow to the
    /// newest active run row. Manual focus moves and explicit viewport paging
    /// flip this off; the next stage transition or run launch flips it back on.
    pub(crate) progress_follow_active: bool,
    /// Snapshot of `messages.len()` taken when tail-follow was last
    /// disengaged. None while following. Used to count missed messages
    /// for the "v N new" badge.
    pub(crate) tail_detach_baseline: Option<usize>,
    pub(crate) body_inner_height: usize,
    pub(crate) body_inner_width: usize,
    pub(crate) split_target: Option<SplitTarget>,
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
    pub(crate) live_summary_cached_mtime: Option<SystemTime>,
    /// Per-process watcher that fires when another instance atomically
    /// publishes a new `models.json` under `paths.cache_root`. The notify
    /// backend handles sub-2-s latency; an internal 60-s mtime poll
    /// covers events the kernel-side notifier dropped. `None` only when
    /// the App is constructed without a watcher (tests).
    pub(crate) cache_watcher: Option<cache::CacheWatcher>,
    pub(crate) pending_drain_deadline: Option<Instant>,
    /// Rewind side effects waiting on the runner-confirmed-dead signal.
    /// Set by [`crate::app::App::apply_op_outcome`] when a rewind lands
    /// while an agent is live; consumed by `finalize_run_record` once the
    /// previous agent dies. See [`PendingRewindApply`].
    pub(crate) pending_rewind_apply: Option<PendingRewindApply>,
    pub(crate) pending_quit_confirmation_run_id: Option<u64>,
    pub(crate) pending_cancel_confirmation: bool,
    pub(crate) interactive_exit_prompt_dismissed_at: Option<(u64, usize)>,
    pub(crate) pending_app_exit: bool,
    pub(crate) pending_shell_command: Option<String>,
    pub(crate) current_run_id: Option<u64>,
    /// Lifecycle FSM. `current_run_id` is the persisted mirror; the FSM owns
    /// `ActiveRun` but the app still needs the run id for UI binding.
    pub(crate) fsm: crate::lifecycle::Fsm,
    /// Compact, round-aware lifecycle [`crate::lifecycle::Stage`] derived from
    /// `state.current_stage` via [`Stage::to_lifecycle_stage`].
    /// Refreshed by [`crate::app::App::refresh_lifecycle_stage`] at every stage
    /// mutation site.
    pub(crate) lifecycle_stage: crate::lifecycle::Stage,
    /// Slot for the lifecycle stage the lifecycle was paused at when the
    /// operator issued `:stop`. None outside `Stopping`/`Idle`-after-stop
    /// transitions. Mirrors `OpsCtx.paused_at_stage` so the scheduler can
    /// avoid relaunching the same stage immediately after a stop.
    pub(crate) paused_at_stage: Option<crate::lifecycle::Stage>,
    /// Operator-decision slots used by the lifecycle. Populated by
    /// operator paths that raise modal decisions (`:stop`, `:retry`,
    /// guard approvals, etc.).
    pub(crate) pending_decisions: crate::lifecycle::PendingDecisions,
    /// Lifecycle scheduler. Owns the [`crate::lifecycle::StageRegistry`]
    /// that operator ops and the per-tick auto-launch path consult.
    pub(crate) scheduler: crate::lifecycle::Scheduler,
    pub(crate) failed_models: HashMap<RetryKey, FailedModelSet>,
    /// Pinned model for the very next launch the scheduler dispatches.
    ///
    /// Used by [`crate::app::retry_policy::App::maybe_auto_retry`] to plumb a
    /// model-fallback choice through the scheduler-driven launch path: the
    /// policy picks the next vendor, parks it here, and lets the next tick's
    /// [`App::maybe_auto_launch`] hand the override to the chosen stage's
    /// `launch_*_with_model` entry point. Consumed-once and cleared on read
    /// so a single override never bleeds across launches.
    pub(crate) next_run_model_override: Option<CachedModel>,
    pub(crate) pending_yolo_toggle_gate: Option<&'static str>,
    pub(crate) yolo_exit_issued: HashSet<u64>,
    pub(crate) yolo_exit_observations: HashMap<u64, YoloExitObservation>,
    pub(crate) runner_supervisor: crate::data::runner::Supervisor,
    /// Runner-level operator knobs (full-alignment cadence, etc.).
    /// Populated from `config.runner_view()` at construction time.
    pub(crate) runner_config: crate::data::runner::RunnerConfig,
    pub(crate) notification_runtime: crate::data::notifications::NotificationRuntime,
    pub(crate) interactive_wait_marker: Option<crate::data::notifications::InteractiveWaitMarker>,
    /// The loaded unified config, shared across subsystems. Every view
    /// (ntfy, ACP, runner, paths, diagnostics, memory, UI) is derived
    /// from this single `Arc<Config>` — load-on-launch, no global static.
    pub(crate) config: std::sync::Arc<crate::data::config::Config>,
    /// Pre-extracted read-only view of `[memory]`.
    pub(crate) memory_view: crate::data::config::view::MemoryView,
    /// Pre-extracted read-only view of `[ui]`.
    pub(crate) ui_view: crate::data::config::view::UiView,
    /// Pre-extracted read-only view of `[paths]` with `$HOME` already
    /// expanded at load time. All path resolution inside `App` and its
    /// stages flows through this struct so operators can independently
    /// override session, run, cache, and memory roots.
    pub(crate) paths: crate::data::config::view::PathsView,
    /// Per-run liveness watchdog state. Allocated as part of task 1
    /// scaffolding; the App-side lifecycle hookup that inserts/removes
    /// entries (and ticks `evaluate`) lands with task 2.
    pub(crate) watchdog: watchdog::WatchdogRegistry,
    #[cfg(test)]
    pub(crate) test_launch_harness: Option<std::sync::Arc<std::sync::Mutex<TestLaunchHarness>>>,
    pub(crate) messages: Vec<Message>,
    pub(crate) messages_observed_state: Option<(SystemTime, u64)>,
    pub(crate) status_line: Rc<RefCell<self::status_line::StatusLine>>,
    pub(crate) prev_models_mode: ModelsAreaMode,
    pub(crate) palette: self::palette::PaletteState,
    pub(crate) command_return_target: Option<CommandReturnTarget>,
    pub(crate) config_panel: Option<self::config_panel::ConfigPanelState>,
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
    pub(crate) project_name: String,
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
