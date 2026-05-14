//! Operator-command surface for the agent lifecycle.
//!
//! [`LifecycleOps`] exposes the four palette operations — `:stop`,
//! `:restart`, `:rewind <target>`, and `:cancel` — as pure-ish functions
//! over an [`OpsCtx`]. Each function decides what the FSM and session-
//! shape mutations should be, builds a [`CleanupPlan`] of file deletes and
//! backup restores, and emits an [`OpOutcome`] the caller drives.
//!
//! Two-phase model:
//! - When the FSM is idle, the operator command resolves *immediately*:
//!   the returned [`OpAction::Immediate`] carries the phase change,
//!   cleanup, pending-decision pruning, and start-spec the caller applies
//!   synchronously.
//! - When the FSM is active, the operator command calls
//!   [`Fsm::request_stop`] inside ops with an [`AfterStop`] variant that
//!   already carries the cleanup and phase change. The caller observes a
//!   [`OpAction::PendingStop`] return value, then later — once the agent
//!   is dead — runs [`Fsm::confirm_dead`] and applies the same plan from
//!   the resolved `next` variant.
//!
//! `resolution_to_action` bridges [`StopResolution`] back to a flat
//! [`OpAction::Immediate`] the App can apply uniformly after a stopped run
//! finalizes.
use super::fsm::{AfterStop, AgentState, CleanupPlan, Fsm, StopResolution};
use super::pending::PendingDecisions;
use super::phase::Phase;
use super::spec::StageSpec;
use super::stage::{StageCtx, StageRegistry};
use std::path::PathBuf;

/// Context bundle the operator commands mutate or read.
///
/// `fsm` / `phase` / `paused_at_phase` / `pending_decisions` are the
/// session-shape fields the ops touch synchronously. `registry` and
/// `stage_ctx` are read-only — the registry to look stages up, the
/// `StageCtx` to feed [`super::Stage::build_spec`] / `next_pending_work`
/// when computing follow-on specs.
pub struct OpsCtx<'a> {
    pub fsm: &'a mut Fsm,
    pub phase: &'a mut Phase,
    pub paused_at_phase: &'a mut Option<Phase>,
    pub pending_decisions: &'a mut PendingDecisions,
    pub registry: &'a StageRegistry,
    pub stage_ctx: StageCtx<'a>,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// Result of dispatching an operator command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpOutcome {
    /// Nothing happened. The wrapped string is the operator-facing reason
    /// — typically displayed as a status-bar warning.
    NoOp(String),
    /// The command staged work the caller still has to drive. See
    /// [`OpAction`] for the two variants.
    Staged(OpAction),
}

/// Side-effect plan returned by [`LifecycleOps`] commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpAction {
    /// Synchronous: no agent was running, so the caller can apply
    /// `phase_change` / `cleanup` / pending-decision pruning immediately
    /// and (if non-`None`) issue `fsm.start(start_spec)`.
    Immediate {
        phase_change: Option<Phase>,
        cleanup: CleanupPlan,
        clear_paused: bool,
        clear_pending: bool,
        start_spec: Option<StageSpec>,
    },
    /// Asynchronous: an agent was active when the command landed, so
    /// `Fsm::request_stop` has been called for the caller. When the
    /// runner confirms the agent is dead, the caller invokes
    /// [`Fsm::confirm_dead`] and applies the same plan; the plan is also
    /// embedded in the [`AfterStop`] variant the FSM is now carrying, so
    /// [`resolution_to_action`] can re-derive an `Immediate` action from
    /// the resolved [`StopResolution`].
    PendingStop {
        after: AfterStop,
        cleanup: CleanupPlan,
        phase_change: Option<Phase>,
        clear_paused: bool,
        clear_pending: bool,
    },
}

/// Single-slot namespace for the four operator commands.
pub struct LifecycleOps;

impl LifecycleOps {
    /// `:stop` — request the active agent stop with no follow-on. Sets
    /// `paused_at_phase = Some(phase)` so the scheduler doesn't immediately
    /// relaunch the same stage; `:restart` and `:rewind` clear this slot.
    ///
    /// Idle FSM → [`OpOutcome::NoOp`]. The "re-request from Stopping"
    /// branch exercises [`Fsm::request_stop`]'s precedence rule (latest
    /// non-Cancel wins; Cancel sticks).
    pub fn stop(ctx: &mut OpsCtx<'_>) -> OpOutcome {
        match ctx.fsm.view() {
            AgentState::Idle => OpOutcome::NoOp("no agent running".to_string()),
            AgentState::Starting { .. } => {
                // Starting has no live run for the FSM to stop; treat the
                // same as Idle so the operator gets a clear message.
                OpOutcome::NoOp("agent has not started yet".to_string())
            }
            AgentState::Running { .. } | AgentState::Stopping { .. } => {
                // The Stopping branch goes through `Fsm::request_stop`'s
                // precedence rules: a fresh GoIdle replaces any prior
                // non-Cancel `after`; an existing Cancel sticks.
                ctx.fsm
                    .request_stop(AfterStop::GoIdle)
                    .expect("running/stopping FSM accepts request_stop");
                *ctx.paused_at_phase = Some(*ctx.phase);
                OpOutcome::Staged(OpAction::PendingStop {
                    after: AfterStop::GoIdle,
                    cleanup: CleanupPlan::empty(),
                    phase_change: None,
                    clear_paused: false,
                    clear_pending: false,
                })
            }
        }
    }

    /// `:restart` — preempt the active agent and relaunch the same stage
    /// with `attempt + 1`.
    ///
    /// Non-restartable stages (validators, idempotent passes) return
    /// `NoOp("stage does not support restart")`.
    pub fn restart(ctx: &mut OpsCtx<'_>) -> OpOutcome {
        let stage_id = match ctx.fsm.view() {
            AgentState::Idle => {
                return OpOutcome::NoOp("no agent running".to_string());
            }
            AgentState::Starting { .. } => {
                // No live run for `Fsm::request_stop` to drive; the app
                // preempts the pending launch via the supervisor. Treat as
                // a no-op here.
                return OpOutcome::NoOp("agent has not started yet".to_string());
            }
            AgentState::Running { run } => run.spec.stage_id,
            AgentState::Stopping { run, .. } => run.spec.stage_id,
        };
        let Some(stage) = ctx.registry.get(stage_id) else {
            return OpOutcome::NoOp(format!("no stage registered for {stage_id:?}"));
        };
        if !stage.supports_restart() {
            return OpOutcome::NoOp("stage does not support restart".to_string());
        }
        let next_spec = stage.build_spec(&ctx.stage_ctx).with_attempt_plus_one();
        ctx.fsm
            .request_stop(AfterStop::Restart {
                spec: next_spec.clone(),
            })
            .expect("active FSM accepts request_stop");
        *ctx.paused_at_phase = None;
        OpOutcome::Staged(OpAction::PendingStop {
            after: AfterStop::Restart { spec: next_spec },
            cleanup: CleanupPlan::empty(),
            phase_change: None,
            clear_paused: true,
            clear_pending: false,
        })
    }

    /// `:rewind <target>` — roll the session [`Phase`] back to `target`,
    /// clean up artifacts for every stage strictly past `target`, restore
    /// the target stage's backups, and auto-launch the next stage at
    /// `target` (if any).
    pub fn rewind(ctx: &mut OpsCtx<'_>, target: Phase) -> OpOutcome {
        // Refuse rewinds that don't go backwards. `Phase`'s `partial_cmp`
        // returns None for `Phase::Cancelled`, which we treat as a no-op
        // since "rewinding past cancelled" is meaningless.
        match target.partial_cmp(ctx.phase) {
            Some(std::cmp::Ordering::Less) => {}
            _ => return OpOutcome::NoOp("nothing to rewind".to_string()),
        }

        let cleanup = build_rewind_cleanup(ctx, target);
        let start_spec = build_start_spec_for_phase(ctx, target);

        match ctx.fsm.view() {
            AgentState::Idle | AgentState::Starting { .. } => {
                // Starting has no live run for `Fsm::request_stop` to
                // drive; lump it in with Idle and apply the rewind
                // synchronously.
                OpOutcome::Staged(OpAction::Immediate {
                    phase_change: Some(target),
                    cleanup,
                    clear_paused: true,
                    clear_pending: true,
                    start_spec,
                })
            }
            AgentState::Running { .. } | AgentState::Stopping { .. } => {
                let after = AfterStop::Rewind {
                    target,
                    spec: start_spec,
                    cleanup: cleanup.clone(),
                    clear_pending: true,
                };
                ctx.fsm
                    .request_stop(after.clone())
                    .expect("active FSM accepts request_stop");
                *ctx.paused_at_phase = None;
                OpOutcome::Staged(OpAction::PendingStop {
                    after,
                    cleanup,
                    phase_change: Some(target),
                    clear_paused: true,
                    clear_pending: true,
                })
            }
        }
    }

    /// `:cancel` — end the session. Stops any active agent and marks the
    /// [`Phase`] as [`Phase::Cancelled`].
    pub fn cancel(ctx: &mut OpsCtx<'_>) -> OpOutcome {
        match ctx.fsm.view() {
            AgentState::Idle | AgentState::Starting { .. } => {
                // Starting has no live run for `Fsm::request_stop` to
                // drive; apply cancellation immediately.
                OpOutcome::Staged(OpAction::Immediate {
                    phase_change: Some(Phase::Cancelled),
                    cleanup: CleanupPlan::empty(),
                    clear_paused: false,
                    clear_pending: true,
                    start_spec: None,
                })
            }
            AgentState::Running { .. } | AgentState::Stopping { .. } => {
                ctx.fsm
                    .request_stop(AfterStop::Cancel)
                    .expect("active FSM accepts request_stop");
                OpOutcome::Staged(OpAction::PendingStop {
                    after: AfterStop::Cancel,
                    cleanup: CleanupPlan::empty(),
                    phase_change: Some(Phase::Cancelled),
                    clear_paused: false,
                    clear_pending: true,
                })
            }
        }
    }
}

/// Lift a [`StopResolution`] back to the same `OpAction::Immediate` shape
/// the idle-path commands return. The confirm-dead handler uses this
/// single function to apply any pending operator command's plan uniformly,
/// regardless of which command produced it.
pub fn resolution_to_action(resolution: StopResolution) -> OpAction {
    match resolution.next {
        AfterStop::GoIdle => OpAction::Immediate {
            phase_change: None,
            cleanup: CleanupPlan::empty(),
            clear_paused: false,
            clear_pending: false,
            start_spec: None,
        },
        AfterStop::Restart { spec } => OpAction::Immediate {
            phase_change: None,
            cleanup: CleanupPlan::empty(),
            clear_paused: true,
            clear_pending: false,
            start_spec: Some(spec),
        },
        AfterStop::Rewind {
            target,
            spec,
            cleanup,
            clear_pending,
        } => OpAction::Immediate {
            phase_change: Some(target),
            cleanup,
            clear_paused: true,
            clear_pending,
            start_spec: spec,
        },
        AfterStop::Cancel => OpAction::Immediate {
            phase_change: Some(Phase::Cancelled),
            cleanup: CleanupPlan::empty(),
            clear_paused: false,
            clear_pending: true,
            start_spec: None,
        },
    }
}

/// Compute the [`CleanupPlan`] for a rewind to `target`.
///
/// Iterates every registered stage whose `phase_when_running` is strictly
/// later than `target` and collects its `artifact_paths` and
/// `prompt_paths` for every round from 1 up to the current round (so a
/// multi-shot stage's per-round directory tree is fully cleaned). The
/// target stage's own `restore_backups` are added so the operator's
/// working tree (e.g. `plan.md` from `plan.pre-review-1.md`) is restored.
/// All paths are joined to [`StageCtx::session_dir`] to become absolute.
fn build_rewind_cleanup(ctx: &OpsCtx<'_>, target: Phase) -> CleanupPlan {
    let mut delete: Vec<PathBuf> = Vec::new();
    let mut restore_backups: Vec<(PathBuf, PathBuf)> = Vec::new();

    let current_round = round_for_phase(*ctx.phase);
    for id in ctx.registry.stages_after(target) {
        let Some(stage) = ctx.registry.get(id) else {
            continue;
        };
        for round in 1..=current_round.max(1) {
            for rel in stage.artifact_paths(round) {
                delete.push(ctx.stage_ctx.session_dir.join(rel));
            }
            for rel in stage.prompt_paths(round) {
                delete.push(ctx.stage_ctx.session_dir.join(rel));
            }
        }
    }

    // The target stage itself isn't in `stages_after`, but its
    // restore_backups are how the operator's working tree gets reset.
    // The "target stage" for restore purposes is the stage whose
    // `next_phase_on_success` *leaves* the lifecycle on `target` — for
    // Phase::Plan that's PlanReview (the plan.pre-review-1.md backup is
    // the canonical example). The simplest mapping is to query every
    // registered stage and pick those whose phase_when_running == target.
    for id in ctx.registry.stages_at_phase(target) {
        let Some(stage) = ctx.registry.get(id) else {
            continue;
        };
        for (backup_rel, dest_rel) in stage.restore_backups(1) {
            restore_backups.push((
                ctx.stage_ctx.session_dir.join(backup_rel),
                ctx.stage_ctx.session_dir.join(dest_rel),
            ));
        }
    }

    CleanupPlan {
        delete,
        restore_backups,
    }
}

/// Build the `start_spec` to fire on rewind to `target`. Walks the
/// per-phase candidate list (matching the canonical pipeline order)
/// and returns the first stage with [`super::Stage::next_pending_work`].
fn build_start_spec_for_phase(ctx: &OpsCtx<'_>, target: Phase) -> Option<StageSpec> {
    let id = ctx.registry.next_stage_for_phase(target, &ctx.stage_ctx)?;
    let stage = ctx.registry.get(id)?;
    Some(stage.build_spec(&ctx.stage_ctx))
}

/// Round number for the lifecycle's current phase. Used by
/// [`build_rewind_cleanup`] to bound the per-round path enumeration so a
/// rewind in round 3 cleans up `rounds/001..rounds/003`.
fn round_for_phase(phase: Phase) -> u32 {
    match phase {
        Phase::Implementation(r) | Phase::Review(r) => r,
        _ => 1,
    }
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod ops_tests;
