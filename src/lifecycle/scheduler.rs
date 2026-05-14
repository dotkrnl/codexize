//! Pure planning scheduler for the lifecycle FSM.
//!
//! [`Scheduler::plan`] is the single decision point: given the agent FSM
//! state, the current [`Stage`], operator gates, and a read-only
//! [`StageCtx`], it returns a [`TickOutcome`] describing what the caller
//! should do next. The function is **pure** — no IO, no FSM mutation —
//! which keeps it trivial to unit-test with hand-built inputs.
//!
//! The caller (App) takes the returned [`StageSpec`] and decides
//! whether to actually invoke [`Fsm::start`](super::Fsm::start). This
//! separation lets project-lane gating, paused-stage gating, and pending
//! decisions all coexist in one place without leaking back into the FSM.
use super::fsm::AgentState;
use super::pending::PendingDecisions;
use super::spec::StageSpec;
use super::stage::{StageCtx, StageRegistry};
use super::stage_state::Stage;

/// Why a tick produced no dispatch decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// The FSM is not [`AgentState::Idle`] — a launch is already in flight,
    /// running, or stopping.
    AgentBusy,
    /// The operator paused this session at exactly its current stage.
    Paused,
    /// A [`PendingDecisions`] slot blocks the current stage.
    PendingDecision,
    /// The project-lane gate (cross-session) denied the dispatch — typically
    /// because another session in the implementation lane is already
    /// running.
    ProjectLane,
    /// The session stage is [`Stage::Done`] or [`Stage::Cancelled`]; the
    /// pipeline cannot advance further.
    Terminal,
}

/// What a single [`Scheduler::plan`] call decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    /// Nothing to launch right now — the stage is awaiting operator input
    /// or every candidate stage has no pending work.
    Idle,
    /// A gate prevented dispatching. The variant carries which gate.
    Blocked(BlockReason),
    /// The caller should hand `spec` to [`Fsm::start`](super::Fsm::start).
    Dispatch(StageSpec),
}

/// Inputs to a single tick. Borrows are read-only; ownership of every
/// piece of state stays with the caller.
#[derive(Debug)]
pub struct TickInput<'a> {
    /// Current FSM state. Only [`AgentState::Idle`] is dispatchable.
    pub agent: &'a AgentState,
    /// Active lifecycle stage.
    pub stage: Stage,
    /// Operator-paused stage, if any. The session is paused only when the
    /// current stage matches.
    pub paused_at_stage: Option<Stage>,
    /// Pending operator-decision slots, consulted via
    /// [`PendingDecisions::blocks`].
    pub pending_decisions: &'a PendingDecisions,
    /// Whether the project-level (cross-session) lane gate currently
    /// permits this session to dispatch.
    pub project_lane_allows: bool,
    /// Read-only stage context.
    pub ctx: StageCtx<'a>,
}

/// Pure planning scheduler.
///
/// Owns a [`StageRegistry`] and exposes [`Scheduler::plan`] — the caller
/// retains every other piece of state and feeds it in fresh on each tick.
#[derive(Debug)]
pub struct Scheduler {
    registry: StageRegistry,
}

impl Scheduler {
    /// Wrap a registry into a scheduler.
    pub fn new(registry: StageRegistry) -> Self {
        Self { registry }
    }

    /// Borrow the underlying [`StageRegistry`]. Useful for callers that need
    /// to look up a specific [`Stage`](super::stage::Stage) (e.g. to invoke
    /// [`Stage::build_spec`](super::stage::Stage::build_spec) after
    /// `:retry`).
    pub fn registry(&self) -> &StageRegistry {
        &self.registry
    }

    /// Compute the next tick decision.
    ///
    /// **Pure**: this function reads from its inputs and the registry only.
    /// It never mutates `self`, never touches the FSM, never performs IO.
    /// The caller acts on the returned [`TickOutcome`].
    ///
    /// Block precedence (highest priority first):
    /// 1. [`BlockReason::AgentBusy`] — a launch/run/stop is already in
    ///    flight; nothing else matters until the FSM clears.
    /// 2. [`BlockReason::Terminal`] — the stage is unreachable.
    /// 3. [`BlockReason::Paused`] — operator paused this session here.
    /// 4. [`BlockReason::PendingDecision`] — a modal is open.
    /// 5. [`BlockReason::ProjectLane`] — cross-session gate denied us.
    ///
    /// After the gates clear, the registry resolves a stage and the
    /// stage builds a spec. The result is [`TickOutcome::Idle`] when the
    /// stage has no candidate with pending work (e.g. a stage just
    /// finished and the FSM hasn't bumped the stage yet).
    pub fn plan(&self, input: TickInput<'_>) -> TickOutcome {
        if !matches!(input.agent, AgentState::Idle) {
            return TickOutcome::Blocked(BlockReason::AgentBusy);
        }
        if input.stage.is_terminal() {
            return TickOutcome::Blocked(BlockReason::Terminal);
        }
        if input.paused_at_stage == Some(input.stage) {
            return TickOutcome::Blocked(BlockReason::Paused);
        }
        if input.pending_decisions.blocks() {
            return TickOutcome::Blocked(BlockReason::PendingDecision);
        }
        if !input.project_lane_allows {
            return TickOutcome::Blocked(BlockReason::ProjectLane);
        }

        let Some(stage_id) = self.registry.next_stage_for_stage(input.stage, &input.ctx) else {
            return TickOutcome::Idle;
        };
        let stage = self
            .registry
            .get(stage_id)
            .expect("registry contract: id from next_stage_for_stage is registered");
        // `next_stage_for_stage` already filtered by `next_pending_work` —
        // re-checking here would be redundant. Build the spec directly.
        TickOutcome::Dispatch(stage.build_spec(&input.ctx))
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
