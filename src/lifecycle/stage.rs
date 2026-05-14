//! Stage trait and registry.
//!
//! Step 1 introduced the trait and registry shapes. Step 2 adds the concrete
//! [`Stage`] impls (under [`super::stages`]) and expands [`StageCtx`] with
//! the read-only borrows those impls actually consume. Step 3 will implement
//! [`StageRegistry::next_stage_for_phase`] and [`StageRegistry::stages_after`]
//! and register the impls.
use super::fsm::Outcome;
use super::phase::Phase;
use super::spec::{ActiveRun, StageSpec};
use super::stage_id::StageId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Minimal per-run history projection a [`Stage`] needs to decide what to
/// queue next or whether a fresh attempt is required.
///
/// Only the fields stage scheduling actually reads land here — the full
/// [`super::RunRecordV2`] carries more (model selection, effort, timestamps).
/// Keeping this projection lean makes it trivial to construct in unit tests
/// for the Stage impls without dragging in the persistence layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHistoryEntry {
    pub stage_id: StageId,
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
    pub outcome: Option<Outcome>,
}

/// Slim read-only view of session state passed to a [`Stage`].
///
/// Every borrow here is read-only and narrowly scoped — a Stage may inspect
/// session paths, the current phase, the prior-run projection, and operator
/// modes, but it never sees the runner supervisor, agent registry, or
/// anything that lets it spawn a process. The FSM owns side effects; the
/// trait describes intent.
#[derive(Debug)]
pub struct StageCtx<'a> {
    /// Session id (`.codexize/runs/<session_id>/`).
    pub session_id: &'a str,
    /// Absolute session directory. Stage methods that return paths must
    /// return paths *relative* to this directory; the rewinder turns them
    /// absolute by joining here.
    pub session_dir: &'a Path,
    /// Active lifecycle [`Phase`].
    pub phase: Phase,
    /// Slim projection of prior runs the stage may inspect for
    /// `next_pending_work`, attempt counts, or "has this stage succeeded
    /// yet" checks.
    pub prior_runs: &'a [RunHistoryEntry],
    /// Ordered list of pending task ids for the current round, oldest first.
    /// Multi-shot stages (Coder/Reviewer) consume this; single-shot stages
    /// may ignore it.
    pub pending_task_ids: &'a [u32],
    /// Operator-toggleable YOLO flag, surfaced for stages that gate
    /// interactive vs. non-interactive launch on it.
    pub yolo: bool,
    /// Operator-toggleable Cheap-model preference. Stages don't pick models
    /// (the FSM does) but they expose this to the spec via `build_spec` so
    /// the FSM's model picker has the same context the stage saw.
    pub cheap: bool,
}

/// Pointer to the next unit of work a stage wants to schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnit {
    pub task_id: Option<u32>,
    pub round: u32,
    pub attempt: u32,
}

/// Successful run payload handed to [`Stage::next_phase_on_success`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessOutcome {
    pub run: ActiveRun,
}

/// Pipeline stage contract.
///
/// One implementation per [`StageId`]. The trait is intentionally
/// dispatch-only — there is no shared default `launch` method because every
/// stage's launch flow differs in nontrivial ways today. `build_spec`
/// describes *what* the FSM should launch; actual process spawning lives
/// outside the stage so impls remain testable.
pub trait Stage: Send + Sync {
    /// Identifier the registry uses to look this stage up.
    fn id(&self) -> StageId;

    /// Operator-facing label.
    fn label(&self) -> &'static str;

    /// Window name used by the terminal layer for this run. Must match the
    /// literal the existing `launch_*` functions emit so Step 5's cutover
    /// preserves operator-visible labels.
    fn window_name(&self, round: u32, task: Option<u32>) -> String;

    /// Build a fresh [`StageSpec`] for this stage from the current context.
    ///
    /// The spec is purely descriptive: the FSM uses it to schedule a launch.
    /// Stages must not perform I/O here.
    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec;

    /// Next pending work unit, if any. `None` means the stage has nothing
    /// queued at this moment (typically because it has already succeeded
    /// at the current phase, or — for multi-shot stages — every task for
    /// the round is Done).
    fn next_pending_work(&self, ctx: &StageCtx<'_>) -> Option<WorkUnit>;

    /// Whether `:retry` is meaningful for this stage. Defaults to `true`;
    /// non-restartable stages (idempotent finalization passes, validators)
    /// override to `false`.
    fn supports_restart(&self) -> bool {
        true
    }

    /// The [`Phase`] the session sits in while a run of this stage is live.
    fn phase_when_running(&self) -> Phase;

    /// Phase to transition to when a run of this stage finishes successfully.
    fn next_phase_on_success(&self, ctx: &StageCtx<'_>, outcome: &SuccessOutcome) -> Phase;

    /// Artifact paths produced by this stage at the given round, **relative
    /// to the session directory**. The rewinder turns each path absolute by
    /// joining with [`StageCtx::session_dir`].
    fn artifact_paths(&self, round: u32) -> Vec<PathBuf>;

    /// Backup → original path pairs used to restore the operator's working
    /// tree when rewinding through this stage. Both paths are **relative to
    /// the session directory**.
    fn restore_backups(&self, round: u32) -> Vec<(PathBuf, PathBuf)>;

    /// Prompt paths for this stage at the given round (used by
    /// `:edit-prompt`), **relative to the session directory**.
    fn prompt_paths(&self, round: u32) -> Vec<PathBuf>;
}

/// Lookup table of [`Stage`] implementations keyed by [`StageId`].
#[derive(Default)]
pub struct StageRegistry {
    stages: HashMap<StageId, Box<dyn Stage>>,
}

impl std::fmt::Debug for StageRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageRegistry")
            .field("len", &self.stages.len())
            .finish()
    }
}

impl StageRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            stages: HashMap::new(),
        }
    }

    /// Register a stage. The last registration for a given [`StageId`] wins;
    /// callers wanting strict no-overwrite semantics should check
    /// [`StageRegistry::get`] first.
    pub fn register(&mut self, stage: Box<dyn Stage>) {
        let id = stage.id();
        self.stages.insert(id, stage);
    }

    /// Look up the stage for `id`, if registered.
    pub fn get(&self, id: StageId) -> Option<&dyn Stage> {
        self.stages.get(&id).map(|s| s.as_ref())
    }

    /// Resolve the next stage to launch given the current session [`Phase`].
    ///
    /// Returns `None` in Step 1; the real implementation lands once the Stage
    /// impls exist.
    // TODO: Step 3 — implement using the registered stages' phase_when_running
    // and next_phase_on_success.
    pub fn next_stage_for_phase(&self, phase: Phase) -> Option<StageId> {
        let _ = phase;
        None
    }

    /// Stages whose `phase_when_running` is strictly later than `phase` —
    /// used by `:rewind` to know which artifacts and prompts to clean up.
    ///
    /// Returns an empty vector in Step 1.
    // TODO: Step 3 — implement once Stage impls register their
    // `phase_when_running` values.
    pub fn stages_after(&self, phase: Phase) -> Vec<StageId> {
        let _ = phase;
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_none_and_empty() {
        let reg = StageRegistry::new();
        assert!(reg.get(StageId::Brainstorm).is_none());
        assert_eq!(reg.next_stage_for_phase(Phase::Idea), None);
        assert!(reg.stages_after(Phase::Idea).is_empty());
    }
}
