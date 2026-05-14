//! Stage trait and registry.
//!
//! Step 1 introduces the trait and registry shapes so the rest of the
//! lifecycle types can name them. The trait is intentionally not implemented
//! anywhere yet; Step 2 wires up real implementations for every existing
//! stage and Step 3 implements [`StageRegistry::next_stage_for_phase`] and
//! [`StageRegistry::stages_after`].
use super::phase::Phase;
use super::spec::{ActiveRun, StageSpec};
use crate::app_runtime::view::StageId;
use std::collections::HashMap;
use std::path::PathBuf;

/// Slim read-only view of [`crate::app::App`] state passed to a [`Stage`].
///
/// Step 1 holds a single placeholder marker so the trait compiles; the real
/// borrowed fields (session state, builder state, …) land in Step 2 as the
/// existing `launch_*` callers get ported onto this trait.
#[derive(Debug, Default)]
pub struct StageCtx<'a> {
    // Placeholder so the lifetime parameter is meaningful even before
    // real borrows land.
    pub _placeholder: std::marker::PhantomData<&'a ()>,
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
/// stage's launch flow differs in nontrivial ways today. Step 2 will add a
/// `launch` method (and the related context) once the launcher functions get
/// migrated.
pub trait Stage: Send + Sync {
    /// Identifier the registry uses to look this stage up.
    fn id(&self) -> StageId;

    /// Operator-facing label.
    fn label(&self) -> &'static str;

    /// Window name used by the terminal layer for this run.
    fn window_name(&self, round: u32, task: Option<u32>) -> String;

    /// Build a fresh [`StageSpec`] for this stage from the current app state.
    fn build_spec(&self, ctx: &StageCtx<'_>) -> StageSpec;

    /// Next pending work unit, if any. `None` means the stage has nothing
    /// queued at this moment.
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

    /// Artifact paths produced by this stage at the given round (used by
    /// rewind cleanup).
    fn artifact_paths(&self, round: u32) -> Vec<PathBuf>;

    /// Backup → original path pairs used to restore the operator's working
    /// tree when rewinding through this stage.
    fn restore_backups(&self, round: u32) -> Vec<(PathBuf, PathBuf)>;

    /// Prompt paths for this stage at the given round (used by `:edit-prompt`).
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
