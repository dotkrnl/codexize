//! Pipeline state types and pure constructors.
//!
//! All persistence/IO methods on [`SessionState`] live in
//! [`crate::data::persistence::session`]. Keeping the struct here lets the
//! logic layer reference the type without pulling in filesystem or process
//! dependencies; the additional `impl` block in `data/persistence` extends it
//! with load/save/log helpers.
use crate::adapters::EffortLevel;
use crate::logic::pipeline::phase::Phase;
use crate::logic::selection::SelectionPhase;
use crate::state::BuilderState;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
/// An event logged to the run's events.toml audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub timestamp: String,
    pub phase: Phase,
    pub message: String,
}
/// Coarse provenance for a `BlockedNeedsUser` transition.
///
/// Persisted as snake_case strings so the value is stable across process
/// restarts and serializable into `session.toml`. `final_validation` is the
/// only origin that unlocks the force-ship `BlockedNeedsUser -> Done`
/// transition; all other origins reject it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockOrigin {
    Brainstorm,
    SpecReview,
    SkipToImpl,
    Planning,
    PlanReview,
    Sharding,
    Implementation,
    Review,
    BuilderRecovery,
    GitGuard,
    FinalValidation,
    Simplification,
    Dreaming,
}
impl BlockOrigin {
    /// Map a `RunRecord.stage` string to its block origin.
    /// Returns `None` for unrecognized stages so callers can fall back to a
    /// safer value (typically the originating phase). The accepted strings
    /// match what `start_run_tracking` writes into `agent_runs`, not the
    /// `StageIO::stage` identifiers (which differ for several stages).
    pub fn for_stage(stage: &str) -> Option<Self> {
        Some(match stage {
            "brainstorm" => Self::Brainstorm,
            "spec-review" => Self::SpecReview,
            "planning" => Self::Planning,
            "plan-review" => Self::PlanReview,
            "sharding" => Self::Sharding,
            "coder" => Self::Implementation,
            "reviewer" => Self::Review,
            "recovery" => Self::BuilderRecovery,
            "final-validation" => Self::FinalValidation,
            "simplifier" => Self::Simplification,
            "dreaming" => Self::Dreaming,
            _ => return None,
        })
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Done,
    Failed,
    FailedUnverified,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Modes {
    #[serde(default)]
    pub yolo: bool,
    #[serde(default)]
    pub cheap: bool,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchModes {
    #[serde(default)]
    pub yolo: bool,
    #[serde(default)]
    pub cheap: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interactive: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
}
impl Modes {
    pub fn launch_snapshot(self) -> LaunchModes {
        LaunchModes {
            yolo: self.yolo,
            cheap: self.cheap,
            interactive: false,
        }
    }
}
impl LaunchModes {
    pub fn effort_for(self, requested: EffortLevel, phase: SelectionPhase) -> EffortLevel {
        if self.cheap {
            EffortLevel::Low
        } else if self.yolo && matches!(phase, SelectionPhase::Idea | SelectionPhase::Planning) {
            EffortLevel::Tough
        } else {
            requested
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: u64,
    pub stage: String,
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
    pub model: String,
    pub vendor: String,
    /// Persisted key retained for schema compatibility with existing runs.
    pub window_name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: RunStatus,
    pub error: Option<String>,
    #[serde(default)]
    pub effort: EffortLevel,
    #[serde(default)]
    pub modes: LaunchModes,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub mount_device_id: Option<u64>,
    #[serde(default)]
    pub section_path: Option<Vec<SectionPart>>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageKind {
    Started,
    Brief,
    UserInput,
    AgentText,
    AgentThought,
    Summary,
    /// A summary that flags non-success verdicts (e.g., reviewer asked
    /// for revisions). Rendered as a warning rather than green success.
    SummaryWarn,
    End,
}
impl MessageKind {
    pub fn visible_with_filters(self, show_agent_text: bool, show_thinking_text: bool) -> bool {
        match self {
            Self::AgentText => show_agent_text,
            Self::AgentThought => show_thinking_text,
            _ => true,
        }
    }
    pub fn visible_with_agent_text_filter(self, show_agent_text: bool) -> bool {
        self.visible_with_filters(show_agent_text, false)
    }
}
/// Captured path for a `RunRecord` describing where in the pipeline tree
/// the run logically lives. Frozen at run-creation time so the renderer
/// can group adjacent runs by identity without trusting session-level
/// counters at read time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SectionPart {
    Idea,
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    Iteration(u32),
    Loop,
    Simplification,
    FinalValidation,
    Dreaming,
    Recovery { round: u32 },
    RecoveryPlanReview { round: u32 },
    RecoverySharding { round: u32 },
    Task(u32),
    Round { n: u32, attempt: u32 },
    Stage(String),
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DreamingDecisionKind {
    /// Final validation explicitly recommended no Dreaming pass.
    ValidatorSkipped,
    /// Final validation suggested Dreaming and the operator has not chosen.
    Pending,
    /// Operator declined the suggested Dreaming pass.
    OperatorSkipped,
    /// Operator chose to run Dreaming; the run phase is intentionally future
    /// work in this version, but the choice is persisted for resume safety.
    OperatorAccepted,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DreamingDecision {
    pub kind: DreamingDecisionKind,
    pub round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageSender {
    System,
    Agent { model: String, vendor: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub run_id: u64,
    pub kind: MessageKind,
    pub sender: MessageSender,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct EventsFile {
    #[serde(default)]
    pub(crate) events: Vec<Event>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct MessagesFile {
    #[serde(default)]
    pub(crate) messages: Vec<Message>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    Stage,
    Task,
    Round,
    Mode,
    AgentRun,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
pub enum NodeStatus {
    #[strum(to_string = "pending")]
    Pending,
    #[strum(to_string = "running")]
    Running,
    #[strum(to_string = "waiting-user")]
    WaitingUser,
    #[strum(to_string = "done")]
    Done,
    #[strum(to_string = "skipped")]
    Skipped,
    #[strum(to_string = "failed")]
    Failed,
    #[strum(to_string = "failed-unverified")]
    FailedUnverified,
}
impl NodeStatus {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        // Kept for backward compatibility until UI callers migrate.
        self.into()
    }
}
#[derive(Debug, Clone)]
pub struct Node {
    pub label: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub summary: String,
    pub children: Vec<Node>,
    pub run_id: Option<u64>,
    pub leaf_run_id: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, strum::EnumIs)]
#[serde(rename_all = "snake_case")]
pub enum PipelineItemStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Approved,
    Revise,
    HumanBlocked,
    AgentPivot,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineItem {
    pub id: u32,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    pub status: PipelineItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive: Option<bool>,
    /// Outer iteration this task belongs to. Original tasks (and stages
    /// without an iteration concept, e.g. recovery sub-pipeline items) are
    /// in iteration 1. Tasks added by a final-validation goal_gap verdict
    /// are in iteration 2, the next goal_gap's tasks in iteration 3, and so
    /// on. The dashboard groups tasks into a separate
    /// (Loop, Simplification, FinalValidation) trio per iteration so the
    /// message timeline stays chronological — round-2 messages from
    /// validator-inserted tasks render after FV[1] instead of mixing into
    /// the original Loop subtree.
    #[serde(default = "default_iteration")]
    pub iteration: u32,
}
fn default_iteration() -> u32 {
    1
}
/// A non-coder run that produced an unauthorized HEAD advance under
/// `GuardMode::AskOperator`. Persisted on `SessionState` until the operator
/// chooses reset or keep so process restarts cannot lose the decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingGuardDecision {
    pub stage: String,
    #[serde(default)]
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
    pub run_id: u64,
    pub captured_head: String,
    pub current_head: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}
/// The persisted state of a single codexize session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: String,
    pub schema_version: u32,
    #[serde(default)]
    pub modes: Modes,
    #[serde(default)]
    pub agent_runs: Vec<RunRecord>,
    pub current_phase: Phase,
    #[serde(default)]
    pub idea_text: Option<String>,
    /// Operator-facing session title — set by the brainstormer once the spec
    /// is drafted. Falls back to truncated `idea_text` for display.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub show_noninteractive_texts: bool,
    #[serde(default)]
    pub show_thinking_texts: bool,
    #[serde(default)]
    pub agent_error: Option<String>,
    /// Builder loop state (empty until sharding completes)
    #[serde(default)]
    pub builder: BuilderState,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub skip_to_impl_rationale: Option<String>,
    #[serde(default)]
    pub skip_to_impl_kind: Option<crate::artifacts::SkipToImplKind>,
    #[serde(default)]
    pub pending_guard_decision: Option<PendingGuardDecision>,
    /// Number of `FinalValidation` runs entered in this session. Increments
    /// on entry; the orchestrator hard-blocks before the 4th run starts.
    #[serde(default)]
    pub validation_attempts: u32,
    /// Number of `Simplification(round)` runs entered, keyed by the round
    /// being simplified. Increments on entry; the orchestrator hard-blocks
    /// before the 4th run for a given round (`SIMPLIFICATION_ATTEMPT_CAP`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub simplification_attempts: BTreeMap<u32, u32>,
    /// Origin of the most recent `BlockedNeedsUser` transition. Cleared when
    /// the session moves out of `BlockedNeedsUser`. The force-ship guard reads
    /// this field to decide whether `BlockedNeedsUser -> Done` is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_origin: Option<BlockOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dreaming_decision: Option<DreamingDecision>,
}
impl SessionState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            schema_version: 3,
            modes: Modes::default(),
            agent_runs: Vec::new(),
            current_phase: Phase::IdeaInput,
            idea_text: None,
            title: None,
            selected_model: None,
            show_noninteractive_texts: false,
            show_thinking_texts: false,
            agent_error: None,
            builder: BuilderState::default(),
            archived: false,
            skip_to_impl_rationale: None,
            skip_to_impl_kind: None,
            pending_guard_decision: None,
            validation_attempts: 0,
            simplification_attempts: BTreeMap::new(),
            block_origin: None,
            dreaming_decision: None,
        }
    }
    /// Return the next available agent_run_id (monotonic within session).
    pub fn next_agent_run_id(&self) -> u64 {
        self.agent_runs.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }
    pub fn launch_modes(&self) -> LaunchModes {
        self.modes.launch_snapshot()
    }
}
/// Root directory for all session state. Honors the `CODEXIZE_ROOT` env var
/// (used by tests to point at a tempdir); defaults to `.codexize` in the
/// current working directory for normal use.
///
/// Under `cfg(test)`, when `CODEXIZE_ROOT` is unset we fall back to a
/// per-process tempdir instead of the cwd-relative `.codexize`. This is a
/// safety net so a test that forgets `with_temp_root` (or wires up a
/// SessionState helper that quietly calls `state.save()`) cannot scribble
/// `notify-session-*`/`task-rounds-*` directories into the host repo. Tests
/// that need a controlled root keep using the existing helpers; this only
/// catches the leakage path.
pub fn codexize_root() -> PathBuf {
    if let Some(value) = std::env::var_os("CODEXIZE_ROOT") {
        return PathBuf::from(value);
    }
    #[cfg(test)]
    {
        test_default_root()
    }
    #[cfg(not(test))]
    {
        PathBuf::from(".codexize")
    }
}
#[cfg(test)]
fn test_default_root() -> PathBuf {
    use std::sync::OnceLock;
    static TEST_DEFAULT_ROOT: OnceLock<PathBuf> = OnceLock::new();
    TEST_DEFAULT_ROOT
        .get_or_init(|| {
            let path = std::env::temp_dir().join(format!(
                "codexize-test-default-{}",
                std::process::id()
            ));
            // Best-effort; downstream callers will surface any actual IO
            // error against the returned path.
            let _ = std::fs::create_dir_all(&path);
            path
        })
        .clone()
}
/// Return the directory path for a given session ID.
pub fn session_dir(session_id: &str) -> PathBuf {
    codexize_root().join("sessions").join(session_id)
}
#[cfg(test)]
pub(crate) struct TestFsLock {
    inner: std::sync::Mutex<()>,
}
#[cfg(test)]
pub(crate) struct TestFsGuard<'a> {
    _inner: Option<std::sync::MutexGuard<'a, ()>>,
    reset_owner: bool,
}
#[cfg(test)]
std::thread_local! {
    static TEST_FS_LOCK_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
impl TestFsLock {
    pub(crate) fn lock(&'static self) -> std::sync::LockResult<TestFsGuard<'static>> {
        // Tests frequently nest filesystem/cwd helpers (e.g. `with_temp_dir` holding
        // the lock while guard code probes git state). A plain `Mutex<()>` would
        // deadlock on re-entry, so we treat "already held on this thread" as a
        // no-op lock acquisition.
        if TEST_FS_LOCK_HELD.with(std::cell::Cell::get) {
            return Ok(TestFsGuard {
                _inner: None,
                reset_owner: false,
            });
        }
        match self.inner.lock() {
            Ok(guard) => {
                TEST_FS_LOCK_HELD.with(|held| held.set(true));
                Ok(TestFsGuard {
                    _inner: Some(guard),
                    reset_owner: true,
                })
            }
            Err(err) => {
                TEST_FS_LOCK_HELD.with(|held| held.set(true));
                Ok(TestFsGuard {
                    _inner: Some(err.into_inner()),
                    reset_owner: true,
                })
            }
        }
    }
}
#[cfg(test)]
impl Drop for TestFsGuard<'_> {
    fn drop(&mut self) {
        if self.reset_owner {
            TEST_FS_LOCK_HELD.with(|held| held.set(false));
        }
    }
}
#[cfg(test)]
pub(crate) fn test_fs_lock() -> &'static TestFsLock {
    use std::sync::OnceLock;
    static TEST_FS_LOCK: OnceLock<TestFsLock> = OnceLock::new();
    TEST_FS_LOCK.get_or_init(|| TestFsLock {
        inner: std::sync::Mutex::new(()),
    })
}
