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
    /// True when the Recovery / RecoveryPlanReview / RecoverySharding chain
    /// has been activated for the current implementation round (i.e. a
    /// reviewer failure escalated the round into recovery). Defaults to
    /// false; Step 5 populates this from session state. Used by
    /// [`StageRegistry::next_stage_for_phase`] to gate recovery stages so
    /// they aren't auto-scheduled in a healthy round.
    pub recovery_active: bool,
    /// True when the reviewer's approval verdict for the current round
    /// requested a simplification pass. Defaults to false; Step 5 populates
    /// it. Used by [`StageRegistry::next_stage_for_phase`] to gate
    /// [`StageId::Simplification`] on the `Phase::Review(r)` candidate list.
    pub simplification_requested: bool,
    /// True when the operator accepted the dreaming-decision modal after
    /// final validation. Defaults to false; Step 5 populates it. Used by
    /// [`StageRegistry::next_stage_for_phase`] to gate
    /// [`StageId::Dreaming`] on the `Phase::Finalization` candidate list.
    pub dreaming_accepted: bool,
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

    /// Resolve the next stage to launch given the current session [`Phase`]
    /// and a read-only [`StageCtx`] projection.
    ///
    /// The function walks a per-phase ordered list of candidate
    /// [`StageId`]s and returns the first whose
    /// [`Stage::next_pending_work`] surfaces work. Recovery and
    /// simplification candidates are gated on
    /// [`StageCtx::recovery_active`] / [`StageCtx::simplification_requested`]
    /// (and dreaming on [`StageCtx::dreaming_accepted`]) so they never
    /// auto-schedule in healthy rounds — the FSM populates those flags from
    /// pending-decision and reviewer-verdict state.
    ///
    /// Returns `None` when:
    /// - the phase has no candidates ([`Phase::Done`], [`Phase::Cancelled`]),
    /// - every candidate at this phase reports "no pending work" (the phase
    ///   should advance — the scheduler reaches this state transiently
    ///   between a successful run and the FSM bumping the phase), or
    /// - the candidate stage isn't registered (callers should treat that as
    ///   a configuration error, but `None` keeps the API total).
    pub fn next_stage_for_phase(&self, phase: Phase, ctx: &StageCtx<'_>) -> Option<StageId> {
        // Hardcoded per-phase candidate order. The ordering is the canonical
        // pipeline sequence within each phase — first candidate "with pending
        // work" wins. Stage impls' `phase_when_running()` confirms the
        // mappings here.
        let candidates: &[(StageId, GateFn)] = match phase {
            Phase::Idea => &[(StageId::Brainstorm, gate_always)],
            Phase::Spec => &[(StageId::SpecReview, gate_always)],
            Phase::Plan => &[
                (StageId::Planning, gate_always),
                (StageId::PlanReview, gate_always),
                (StageId::RepoStateUpdate, gate_always),
                (StageId::Sharding, gate_always),
            ],
            Phase::Implementation(_) => &[
                (StageId::Coder, gate_always),
                (StageId::Recovery, gate_recovery_active),
                (StageId::RecoveryPlanReview, gate_recovery_active),
                (StageId::RecoverySharding, gate_recovery_active),
            ],
            Phase::Review(_) => &[
                (StageId::Reviewer, gate_always),
                (StageId::Simplification, gate_simplification_requested),
            ],
            Phase::Finalization => &[
                (StageId::FinalValidation, gate_always),
                (StageId::Dreaming, gate_dreaming_accepted),
            ],
            Phase::Done | Phase::Cancelled => &[],
        };

        for (id, gate) in candidates {
            if !gate(ctx) {
                continue;
            }
            let stage = self.stages.get(id)?;
            if stage.next_pending_work(ctx).is_some() {
                return Some(*id);
            }
        }
        None
    }

    /// Stages whose `phase_when_running` is strictly later than `phase` —
    /// used by `:rewind` to know which artifacts and prompts to clean up.
    ///
    /// Compares each registered stage's [`Stage::phase_when_running`]
    /// (a canonical phase key — `Implementation(1)` and `Review(1)` stand in
    /// for every round of their respective lanes) against `phase` using
    /// [`Phase`]'s [`PartialOrd`]. Returns stages with strictly-greater
    /// phases, sorted by phase descending (later phases first) so callers
    /// can iterate latest-stage-first while cleaning up artifacts.
    /// [`Phase::Cancelled`] is incomparable with every other phase; this
    /// function returns an empty vector when called with it.
    pub fn stages_after(&self, phase: Phase) -> Vec<StageId> {
        let mut hits: Vec<(StageId, Phase)> = self
            .stages
            .iter()
            .filter_map(|(id, stage)| {
                let s_phase = stage.phase_when_running();
                match s_phase.partial_cmp(&phase) {
                    Some(std::cmp::Ordering::Greater) => Some((*id, s_phase)),
                    _ => None,
                }
            })
            .collect();
        // Sort by phase descending; ties broken by StageId discriminant so
        // the result is deterministic across runs. The tie-break order is
        // intentionally not part of the public contract.
        hits.sort_by(|a, b| {
            match b.1.partial_cmp(&a.1) {
                Some(ord) => ord,
                None => std::cmp::Ordering::Equal,
            }
            .then_with(|| (a.0 as u32).cmp(&(b.0 as u32)))
        });
        hits.into_iter().map(|(id, _)| id).collect()
    }
}

type GateFn = fn(&StageCtx<'_>) -> bool;

fn gate_always(_ctx: &StageCtx<'_>) -> bool {
    true
}

fn gate_recovery_active(ctx: &StageCtx<'_>) -> bool {
    ctx.recovery_active
}

fn gate_simplification_requested(ctx: &StageCtx<'_>) -> bool {
    ctx.simplification_requested
}

fn gate_dreaming_accepted(ctx: &StageCtx<'_>) -> bool {
    ctx.dreaming_accepted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ctx<'a>(phase: Phase) -> StageCtx<'a> {
        StageCtx {
            session_id: "s",
            session_dir: Path::new("/tmp"),
            phase,
            prior_runs: &[],
            pending_task_ids: &[],
            yolo: false,
            cheap: false,
            recovery_active: false,
            simplification_requested: false,
            dreaming_accepted: false,
        }
    }

    #[test]
    fn empty_registry_returns_none_and_empty() {
        let reg = StageRegistry::new();
        assert!(reg.get(StageId::Brainstorm).is_none());
        assert_eq!(
            reg.next_stage_for_phase(Phase::Idea, &empty_ctx(Phase::Idea)),
            None
        );
        assert!(reg.stages_after(Phase::Idea).is_empty());
    }

    #[test]
    fn empty_registry_returns_none_for_every_phase() {
        let reg = StageRegistry::new();
        for phase in [
            Phase::Idea,
            Phase::Spec,
            Phase::Plan,
            Phase::Implementation(1),
            Phase::Implementation(5),
            Phase::Review(1),
            Phase::Review(5),
            Phase::Finalization,
            Phase::Done,
            Phase::Cancelled,
        ] {
            assert_eq!(
                reg.next_stage_for_phase(phase, &empty_ctx(phase)),
                None,
                "empty registry must return None for {phase:?}"
            );
        }
    }
}
