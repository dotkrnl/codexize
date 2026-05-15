//! Persisting wrappers around the pure logic-layer transition mutators.
//!
//! These helpers log + save to disk after applying an in-memory mutation
//! defined in [`crate::logic::pipeline::transitions`]. Callers in the runtime
//! and tests should prefer these wrappers; tests that need a pure mutation
//! can still call the logic-layer counterpart directly.
use crate::data::adapters::EffortLevel;
use crate::logic::pipeline::stage::Stage;
use crate::logic::pipeline::transitions::{
    FinishedRunRecord, SIMPLIFICATION_ATTEMPT_CAP, VALIDATION_ATTEMPT_CAP, validate_transition,
};
use crate::state::{BlockOrigin, LaunchModes, RunStatus, SectionPart, SessionState};
use anyhow::{Context, Result};
use chrono::Utc;
/// Execute a validated transition, updating the state and persisting it.
///
/// Force-ship guard: `BlockedNeedsUser -> Done` is rejected at runtime unless
/// the current `block_origin` is `FinalValidation`. The static stage graph
/// allows the edge so the operator-facing affordance can be surfaced, but
/// only final-validation blocks may take it.
pub fn execute_transition(state: &mut SessionState, to: Stage) -> Result<()> {
    validate_transition(&state.current_stage, &to).map_err(|e| anyhow::anyhow!("{e}"))?;
    if matches!(state.current_stage, Stage::BlockedNeedsUser)
        && matches!(to, Stage::Done)
        && state.block_origin != Some(BlockOrigin::FinalValidation)
    {
        anyhow::bail!(
            "force-ship from BlockedNeedsUser to Done requires block_origin = final_validation (current: {:?})",
            state.block_origin
        );
    }
    let old_stage = state.current_stage;
    state.current_stage = to;
    // `block_origin` describes the *current* block. Clear it whenever the
    // session leaves `BlockedNeedsUser` so a subsequent re-block must set a
    // fresh origin and stale provenance can never satisfy the force-ship
    // guard above.
    if matches!(old_stage, Stage::BlockedNeedsUser) && !matches!(to, Stage::BlockedNeedsUser) {
        state.block_origin = None;
    }
    state
        .log_event(format!("transitioned stage from {old_stage:?} to {to:?}"))
        .context("failed to log transition event")?;
    state
        .save()
        .context("failed to save state after transition")?;
    Ok(())
}
/// Set `block_origin` and transition to `BlockedNeedsUser`. The single throat
/// for entering a block — every code path that would have called
/// `execute_transition(state, Stage::BlockedNeedsUser)` should call this
/// instead so the persisted provenance is always populated.
pub fn block_with_origin(state: &mut SessionState, origin: BlockOrigin) -> Result<()> {
    state.block_origin = Some(origin);
    execute_transition(state, Stage::BlockedNeedsUser)
}
/// Outcome of [`enter_final_validation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalValidationEntry {
    /// The session entered `FinalValidation(round)`. `attempt` is the
    /// 1-indexed validation attempt that just started (1, 2, or 3).
    Entered { attempt: u32 },
    /// The cap was already at the limit on entry; the session was routed
    /// straight to `BlockedNeedsUser` with `block_origin = FinalValidation`
    /// and the validator must not be launched.
    CapExceeded,
}
/// Single throat for entering `FinalValidation(round)`. Increments
/// `validation_attempts` on success; on the 4th attempt (cap already
/// exhausted) blocks instead so the validator never spawns. Callers MUST
/// gate the validator launch on `Entered`.
pub fn enter_final_validation(
    state: &mut SessionState,
    round: u32,
) -> Result<FinalValidationEntry> {
    if state.validation_attempts >= VALIDATION_ATTEMPT_CAP {
        block_with_origin(state, BlockOrigin::FinalValidation)?;
        return Ok(FinalValidationEntry::CapExceeded);
    }
    let target = Stage::FinalValidation(round);
    // Validate before incrementing so an illegal source stage cannot leak a
    // stale attempt count into the persisted state.
    validate_transition(&state.current_stage, &target).map_err(|e| anyhow::anyhow!("{e}"))?;
    state.validation_attempts += 1;
    let attempt = state.validation_attempts;
    execute_transition(state, target)?;
    Ok(FinalValidationEntry::Entered { attempt })
}
/// Outcome of [`enter_simplification`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimplificationEntry {
    /// The session entered `Simplification(round)`. `attempt` is the
    /// 1-indexed simplifier attempt for this round (1, 2, or 3).
    Entered { attempt: u32 },
    /// The cap for this round was already at the limit on entry; the
    /// session was routed to `BlockedNeedsUser` with
    /// `block_origin = Simplification` and the simplifier must not be
    /// launched.
    CapExceeded,
}
/// Single throat for entering `Simplification(round)`. Increments the
/// per-round entry in `simplification_attempts` on success; on the 4th
/// attempt for the same round (cap already exhausted) blocks instead so the
/// simplifier never spawns. Callers MUST gate the simplifier launch on
/// `Entered`.
pub fn enter_simplification(state: &mut SessionState, round: u32) -> Result<SimplificationEntry> {
    let attempts = state
        .simplification_attempts
        .get(&round)
        .copied()
        .unwrap_or(0);
    if attempts >= SIMPLIFICATION_ATTEMPT_CAP {
        block_with_origin(state, BlockOrigin::Simplification)?;
        return Ok(SimplificationEntry::CapExceeded);
    }
    let target = Stage::Simplification(round);
    // Validate before incrementing so an illegal source stage cannot leak a
    // stale attempt count into the persisted state.
    validate_transition(&state.current_stage, &target).map_err(|e| anyhow::anyhow!("{e}"))?;
    let next = attempts + 1;
    state.simplification_attempts.insert(round, next);
    execute_transition(state, target)?;
    Ok(SimplificationEntry::Entered { attempt: next })
}
/// Compute the section path for a new agent run at creation time.
///
/// The path is frozen once here so the renderer can group runs by structural
/// identity without re-deriving it from mutable session counters at read time.
fn compute_section_path(
    state: &SessionState,
    stage: &str,
    task_id: Option<u32>,
    round: u32,
    attempt: u32,
) -> Vec<SectionPart> {
    let mut path: Vec<SectionPart> = match stage {
        "brainstorm" => vec![SectionPart::Brainstorm],
        "spec-review" => vec![SectionPart::SpecReview],
        "planning" => vec![SectionPart::Planning],
        "plan-review" if matches!(state.current_stage, Stage::BuilderRecoveryPlanReview(_)) => {
            vec![
                SectionPart::Iteration(recovery_iteration_for_path(state, task_id)),
                SectionPart::RecoveryPlanReview { round },
            ]
        }
        "plan-review" => vec![SectionPart::PlanReview],
        "sharding" if matches!(state.current_stage, Stage::BuilderRecoverySharding(_)) => vec![
            SectionPart::Iteration(recovery_iteration_for_path(state, task_id)),
            SectionPart::RecoverySharding { round },
        ],
        "sharding" => vec![SectionPart::Sharding],
        "recovery" => vec![
            SectionPart::Iteration(recovery_iteration_for_path(state, task_id)),
            SectionPart::Recovery { round },
        ],
        "simplifier" => vec![
            SectionPart::Iteration(loop_iteration_for_round(state, round)),
            SectionPart::Simplification,
            SectionPart::Round { n: round, attempt },
        ],
        "final-validation" => vec![
            SectionPart::Iteration(loop_iteration_for_round(state, round)),
            SectionPart::FinalValidation,
            SectionPart::Round { n: round, attempt },
        ],
        "dreaming" => vec![
            SectionPart::Dreaming,
            SectionPart::Round { n: round, attempt },
        ],
        "coder" | "reviewer" => {
            let iteration = task_id
                .and_then(|tid| {
                    state
                        .builder
                        .pipeline_items
                        .iter()
                        .find(|i| i.stage == "coder" && i.task_id == Some(tid))
                        .map(|i| i.iteration)
                })
                .unwrap_or(1);
            let mut head = vec![SectionPart::Iteration(iteration), SectionPart::Loop];
            if let Some(tid) = task_id {
                head.push(SectionPart::Task(tid));
            }
            head.push(SectionPart::Round { n: round, attempt });
            head
        }
        _ => Vec::new(),
    };
    path.push(SectionPart::Stage(stage.to_string()));
    path
}
/// Find the iteration number for a round based on pipeline items.
///
/// Used for simplifier/final-validation which are not tied to a specific task.
fn loop_iteration_for_round(state: &SessionState, round: u32) -> u32 {
    state
        .builder
        .pipeline_items
        .iter()
        .filter(|i| i.round == Some(round))
        .map(|i| i.iteration)
        .max()
        .unwrap_or(1)
}
/// Determine the outer iteration number for a recovery stage run.
///
/// Peeks at `next_iteration_for_recovery` without consuming it: the override
/// must survive for B2's `recovery_outer_iteration` consumer which also reads
/// it. Falls back to the task's own iteration, then the session maximum.
fn recovery_iteration_for_path(state: &SessionState, task_id: Option<u32>) -> u32 {
    if let Some(override_iter) = state.builder.next_iteration_for_recovery {
        return override_iter;
    }
    if let Some(tid) = task_id.or(state.builder.recovery_trigger_task_id)
        && let Some(item) = state
            .builder
            .pipeline_items
            .iter()
            .find(|i| i.stage == "coder" && i.task_id == Some(tid))
    {
        return item.iteration;
    }
    state
        .builder
        .pipeline_items
        .iter()
        .map(|i| i.iteration)
        .max()
        .unwrap_or(1)
}
#[allow(clippy::too_many_arguments)]
pub fn start_agent_run_with_id(
    state: &mut SessionState,
    run_id: u64,
    stage: String,
    task_id: Option<u32>,
    round: u32,
    attempt: u32,
    model: String,
    subscription_label: String,
    window_name: String,
    effort: EffortLevel,
    effort_mapping: crate::data::config::schema::EffortMapping,
    effort_eligible: bool,
    modes: LaunchModes,
) -> u64 {
    let path = compute_section_path(state, &stage, task_id, round, attempt);
    state.create_run_record_with_id(
        run_id,
        stage,
        task_id,
        round,
        attempt,
        model,
        subscription_label,
        window_name,
        effort,
        effort_mapping,
        effort_eligible,
        modes,
        Some(path),
    )
}
pub fn finish_run_record(
    state: &mut SessionState,
    run_id: u64,
    success: bool,
    error: Option<String>,
) -> Option<FinishedRunRecord> {
    let run = state.agent_runs.iter_mut().find(|run| run.id == run_id)?;
    let ended_at = Utc::now();
    run.ended_at = Some(ended_at);
    let unverified = error
        .as_deref()
        .is_some_and(|reason| reason.starts_with("failed_unverified:"));
    run.status = if success {
        RunStatus::Done
    } else if unverified {
        RunStatus::FailedUnverified
    } else {
        RunStatus::Failed
    };
    run.error = error.clone();
    Some(FinishedRunRecord {
        ended_at,
        started_at: run.started_at,
        attempt: run.attempt,
        model: run.model.clone(),
        subscription_label: run.subscription_label.clone(),
        unverified,
        error,
    })
}
pub fn resume_running_runs(state: &mut SessionState) -> Result<Option<u64>> {
    state.resume_running_runs()
}
#[cfg(test)]
#[path = "transitions_tests.rs"]
mod tests;
