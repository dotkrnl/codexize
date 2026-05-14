//! Runtime FSM for the agent lifecycle.
//!
//! [`Fsm`] enforces legal transitions between [`AgentState`] variants. It is
//! intentionally storage-free — nothing here touches disk. Persistence lives
//! in [`super::persist`].
use super::phase::Phase;
use super::spec::{ActiveRun, StageSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Side-effect plan attached to an [`AfterStop`] variant.
///
/// `delete` entries are removed (recursively for directories). `restore_backups`
/// entries are `(backup, dest)` pairs — when the backup exists, it is moved to
/// `dest`. Every path here is absolute; [`super::ops::LifecycleOps`] is the
/// builder, and the cutover handler (Step 5) is the applier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupPlan {
    pub delete: Vec<PathBuf>,
    pub restore_backups: Vec<(PathBuf, PathBuf)>,
}

impl CleanupPlan {
    /// Construct an empty plan — used by operator commands whose intent
    /// carries no artifact cleanup (`:stop`, `:cancel`).
    pub fn empty() -> Self {
        Self::default()
    }

    /// True when no deletes and no restores are queued.
    pub fn is_empty(&self) -> bool {
        self.delete.is_empty() && self.restore_backups.is_empty()
    }
}

/// Runtime state of the single agent slot.
///
/// `Idle` is the only state that accepts a fresh [`Fsm::start`]; every other
/// state already owns either a pending launch (`Starting`) or a live run
/// (`Running`/`Stopping`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Starting { spec: StageSpec },
    Running { run: ActiveRun },
    Stopping { run: ActiveRun, after: AfterStop },
}

/// What the FSM should do once the currently-stopping run finishes.
///
/// Mutated in place by [`Fsm::request_stop`] when a second stop request
/// arrives mid-shutdown — see the precedence rules on that method.
///
/// `Rewind` carries an optional `spec` (None when the target phase has no
/// next stage to schedule, e.g., rewinding to [`Phase::Idea`]), the
/// [`CleanupPlan`] to apply once the run is dead, and a `clear_pending` flag
/// so the caller's confirm-dead handler can drop pending decisions that no
/// longer apply at the rewound phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterStop {
    /// Plain stop. The FSM returns to [`AgentState::Idle`].
    GoIdle,
    /// Restart the same stage with `attempt + 1`. The FSM transitions to
    /// [`AgentState::Starting`] with the supplied [`StageSpec`].
    Restart { spec: StageSpec },
    /// Roll the session [`Phase`] back to `target`, then start the supplied
    /// spec (if any). The actual phase rewind, file cleanup, and pending-
    /// decision pruning live outside the FSM; this variant records the
    /// operator's intent so the resolution carries everything back to the
    /// caller's confirm-dead handler.
    Rewind {
        target: Phase,
        spec: Option<StageSpec>,
        cleanup: CleanupPlan,
        clear_pending: bool,
    },
    /// Mark the session [`Phase::Cancelled`]. Once requested, this outcome
    /// cannot be downgraded — see [`Fsm::request_stop`].
    Cancel,
}

/// Why a run was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelledBy {
    Operator,
    System,
}

/// Terminal outcome for a single agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Done,
    Failed(String),
    FailedUnverified(String),
    Cancelled { by: CancelledBy, reason: String },
    /// Backfilled on resume when the TUI crashed mid-run and we cannot tell
    /// whether the agent actually succeeded.
    Aborted,
    /// This attempt was preempted to make way for a fresh attempt; the
    /// follow-on attempt carries the operator's intent forward.
    Restarted,
}

/// A finalized run record: the [`ActiveRun`] plus its terminal outcome and
/// end timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizedRun {
    pub run: ActiveRun,
    pub outcome: Outcome,
    pub ended_at: chrono::DateTime<chrono::Utc>,
}

/// What [`Fsm::confirm_dead`] resolves to.
///
/// `next` is the [`AfterStop`] the caller should act on — start a new spec,
/// go idle, cancel the session, etc. `finalized` carries the run that just
/// terminated so the caller can persist it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopResolution {
    pub outcome: Outcome,
    pub next: AfterStop,
    pub finalized: FinalizedRun,
}

/// Errors returned by [`Fsm`] when a caller attempts an illegal transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FsmError {
    /// `start()` called while the FSM is not [`AgentState::Idle`].
    #[error("agent is already active")]
    AlreadyActive,
    /// `request_stop()` called while the FSM is [`AgentState::Idle`].
    #[error("agent is not active")]
    NotActive,
    /// `confirm_running()` called from a state other than `Starting`, or
    /// `confirm_dead()` called from a state other than `Stopping`.
    #[error("transition is misordered for the current state")]
    Misordered,
}

/// Single-slot agent FSM.
#[derive(Debug, Clone)]
pub struct Fsm {
    state: AgentState,
}

impl Default for Fsm {
    fn default() -> Self {
        Self::new()
    }
}

impl Fsm {
    /// Construct a fresh FSM in [`AgentState::Idle`].
    pub fn new() -> Self {
        Self {
            state: AgentState::Idle,
        }
    }

    /// Read-only view of the current state.
    pub fn view(&self) -> &AgentState {
        &self.state
    }

    /// Begin launching a new run. Only legal from [`AgentState::Idle`].
    pub fn start(&mut self, spec: StageSpec) -> Result<(), FsmError> {
        match self.state {
            AgentState::Idle => {
                self.state = AgentState::Starting { spec };
                Ok(())
            }
            AgentState::Starting { .. } | AgentState::Running { .. } | AgentState::Stopping { .. } => {
                Err(FsmError::AlreadyActive)
            }
        }
    }

    /// Mark the pending launch as live. Only legal from [`AgentState::Starting`].
    pub fn confirm_running(&mut self, run: ActiveRun) -> Result<(), FsmError> {
        match std::mem::replace(&mut self.state, AgentState::Idle) {
            AgentState::Starting { .. } => {
                self.state = AgentState::Running { run };
                Ok(())
            }
            other => {
                // Restore the prior state on rejection so the FSM remains
                // observable as it was before the failed call.
                self.state = other;
                Err(FsmError::Misordered)
            }
        }
    }

    /// Request the running run be stopped, with a follow-on intent.
    ///
    /// Stop-precedence when already in [`AgentState::Stopping`]:
    /// - [`AfterStop::Cancel`] always wins — once requested, it cannot be
    ///   downgraded by a later request, even one that also pushes a Cancel
    ///   (the existing Cancel is preserved).
    /// - Otherwise, the latest request replaces the previous `after` — a
    ///   later `GoIdle` wins over an earlier `Restart`, and so on.
    pub fn request_stop(&mut self, after: AfterStop) -> Result<(), FsmError> {
        match std::mem::replace(&mut self.state, AgentState::Idle) {
            AgentState::Idle => {
                // Restore (it was already Idle, but be explicit) and reject.
                self.state = AgentState::Idle;
                Err(FsmError::NotActive)
            }
            AgentState::Starting { spec } => {
                // No live run yet — restore Starting and reject; the caller
                // should cancel the pending launch via a different path.
                // Step 4 will replace this with a proper preempt path; for
                // now we model "not active" as "no Running/Stopping run".
                self.state = AgentState::Starting { spec };
                Err(FsmError::NotActive)
            }
            AgentState::Running { run } => {
                self.state = AgentState::Stopping { run, after };
                Ok(())
            }
            AgentState::Stopping { run, after: prev } => {
                let resolved = match (prev, after) {
                    // Cancel is sticky.
                    (AfterStop::Cancel, _) => AfterStop::Cancel,
                    // Latest non-Cancel request replaces the previous.
                    (_, next) => next,
                };
                self.state = AgentState::Stopping { run, after: resolved };
                Ok(())
            }
        }
    }

    /// Mark the stopping run as dead and resolve the follow-on intent.
    /// Only legal from [`AgentState::Stopping`].
    pub fn confirm_dead(&mut self, outcome: Outcome) -> Result<StopResolution, FsmError> {
        match std::mem::replace(&mut self.state, AgentState::Idle) {
            AgentState::Stopping { run, after } => {
                let finalized = FinalizedRun {
                    run,
                    outcome: outcome.clone(),
                    ended_at: chrono::Utc::now(),
                };
                // Move the FSM to the appropriate follow-on state. The phase
                // rewind, file cleanup, and pending-decision pruning in
                // `AfterStop::Rewind` are the caller's job; the FSM only sets
                // itself up to launch the supplied spec next (when there is
                // one — rewinding to a phase with no follow-on stage parks
                // the FSM in [`AgentState::Idle`]).
                self.state = match &after {
                    AfterStop::GoIdle | AfterStop::Cancel => AgentState::Idle,
                    AfterStop::Restart { spec } => AgentState::Starting { spec: spec.clone() },
                    AfterStop::Rewind { spec: Some(spec), .. } => AgentState::Starting {
                        spec: spec.clone(),
                    },
                    AfterStop::Rewind { spec: None, .. } => AgentState::Idle,
                };
                Ok(StopResolution {
                    outcome,
                    next: after,
                    finalized,
                })
            }
            other => {
                self.state = other;
                Err(FsmError::Misordered)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::StageId;

    fn spec(stage: StageId, attempt: u32) -> StageSpec {
        StageSpec {
            stage_id: stage,
            round: 1,
            task_id: None,
            attempt,
            window_name: format!("{stage:?}-{attempt}").to_lowercase(),
        }
    }

    fn active(spec: StageSpec, run_id: u64) -> ActiveRun {
        ActiveRun {
            run_id,
            spec,
            started_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn start_from_idle_succeeds() {
        let mut fsm = Fsm::new();
        assert!(matches!(fsm.view(), AgentState::Idle));
        fsm.start(spec(StageId::Brainstorm, 1)).expect("start ok");
        assert!(matches!(fsm.view(), AgentState::Starting { .. }));
    }

    #[test]
    fn start_from_running_returns_already_active() {
        let mut fsm = Fsm::new();
        let s = spec(StageId::Brainstorm, 1);
        fsm.start(s.clone()).unwrap();
        fsm.confirm_running(active(s.clone(), 1)).unwrap();
        let err = fsm.start(spec(StageId::SpecReview, 1)).unwrap_err();
        assert_eq!(err, FsmError::AlreadyActive);
    }

    #[test]
    fn request_stop_from_idle_returns_not_active() {
        let mut fsm = Fsm::new();
        let err = fsm.request_stop(AfterStop::GoIdle).unwrap_err();
        assert_eq!(err, FsmError::NotActive);
    }

    #[test]
    fn confirm_dead_from_running_returns_misordered() {
        let mut fsm = Fsm::new();
        let s = spec(StageId::Brainstorm, 1);
        fsm.start(s.clone()).unwrap();
        fsm.confirm_running(active(s, 1)).unwrap();
        let err = fsm.confirm_dead(Outcome::Done).unwrap_err();
        assert_eq!(err, FsmError::Misordered);
    }

    #[test]
    fn happy_path_returns_stop_resolution() {
        let mut fsm = Fsm::new();
        let s = spec(StageId::Brainstorm, 1);
        fsm.start(s.clone()).unwrap();
        fsm.confirm_running(active(s.clone(), 7)).unwrap();
        fsm.request_stop(AfterStop::GoIdle).unwrap();
        let res = fsm.confirm_dead(Outcome::Done).unwrap();
        assert_eq!(res.outcome, Outcome::Done);
        assert_eq!(res.next, AfterStop::GoIdle);
        assert_eq!(res.finalized.run.run_id, 7);
        assert_eq!(res.finalized.outcome, Outcome::Done);
        assert!(matches!(fsm.view(), AgentState::Idle));
    }

    #[test]
    fn stop_precedence_go_idle_replaces_restart() {
        let mut fsm = Fsm::new();
        let s = spec(StageId::Brainstorm, 1);
        fsm.start(s.clone()).unwrap();
        fsm.confirm_running(active(s.clone(), 1)).unwrap();

        let restart = AfterStop::Restart {
            spec: spec(StageId::Brainstorm, 2),
        };
        fsm.request_stop(restart).unwrap();
        fsm.request_stop(AfterStop::GoIdle).unwrap();

        match fsm.view() {
            AgentState::Stopping { after, .. } => assert_eq!(after, &AfterStop::GoIdle),
            other => panic!("expected Stopping, got {other:?}"),
        }
    }

    #[test]
    fn stop_precedence_cancel_beats_restart_and_later_go_idle() {
        let mut fsm = Fsm::new();
        let s = spec(StageId::Brainstorm, 1);
        fsm.start(s.clone()).unwrap();
        fsm.confirm_running(active(s.clone(), 1)).unwrap();

        let restart = AfterStop::Restart {
            spec: spec(StageId::Brainstorm, 2),
        };
        fsm.request_stop(restart).unwrap();
        fsm.request_stop(AfterStop::Cancel).unwrap();
        // A later GoIdle must not downgrade a sticky Cancel.
        fsm.request_stop(AfterStop::GoIdle).unwrap();

        match fsm.view() {
            AgentState::Stopping { after, .. } => assert_eq!(after, &AfterStop::Cancel),
            other => panic!("expected Stopping, got {other:?}"),
        }
    }

    #[test]
    fn confirm_dead_with_restart_transitions_to_starting() {
        let mut fsm = Fsm::new();
        let s = spec(StageId::Brainstorm, 1);
        fsm.start(s.clone()).unwrap();
        fsm.confirm_running(active(s.clone(), 1)).unwrap();

        let next_spec = spec(StageId::Brainstorm, 2);
        fsm.request_stop(AfterStop::Restart {
            spec: next_spec.clone(),
        })
        .unwrap();
        let res = fsm
            .confirm_dead(Outcome::Cancelled {
                by: CancelledBy::Operator,
                reason: "retry".into(),
            })
            .unwrap();
        assert!(matches!(res.next, AfterStop::Restart { .. }));
        match fsm.view() {
            AgentState::Starting { spec } => assert_eq!(spec, &next_spec),
            other => panic!("expected Starting, got {other:?}"),
        }
    }
}
