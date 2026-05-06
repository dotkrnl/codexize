//! Persisting wrappers around the pure logic-layer transition mutators.
//!
//! These helpers log + save to disk after applying an in-memory mutation
//! defined in [`crate::logic::pipeline::transitions`]. Callers in the runtime
//! and tests should prefer these wrappers; tests that need a pure mutation
//! can still call the logic-layer counterpart directly.

use crate::adapters::EffortLevel;
use crate::logic::pipeline::phase::Phase;
use crate::logic::pipeline::state::{
    BlockOrigin, LaunchModes, RunStatus, SectionPart, SessionState,
};
use crate::logic::pipeline::transitions::{
    FinishedRunRecord, SIMPLIFICATION_ATTEMPT_CAP, VALIDATION_ATTEMPT_CAP, validate_transition,
};
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::Path;

/// Execute a validated transition, updating the state and persisting it.
///
/// Force-ship guard: `BlockedNeedsUser -> Done` is rejected at runtime unless
/// the current `block_origin` is `FinalValidation`. The static phase graph
/// allows the edge so the operator-facing affordance can be surfaced, but
/// only final-validation blocks may take it.
pub fn execute_transition(state: &mut SessionState, to: Phase) -> Result<()> {
    validate_transition(&state.current_phase, &to).map_err(|e| anyhow::anyhow!("{e}"))?;

    if matches!(state.current_phase, Phase::BlockedNeedsUser)
        && matches!(to, Phase::Done)
        && state.block_origin != Some(BlockOrigin::FinalValidation)
    {
        anyhow::bail!(
            "force-ship from BlockedNeedsUser to Done requires block_origin = final_validation (current: {:?})",
            state.block_origin
        );
    }

    let old_phase = state.current_phase;
    state.current_phase = to;

    // `block_origin` describes the *current* block. Clear it whenever the
    // session leaves `BlockedNeedsUser` so a subsequent re-block must set a
    // fresh origin and stale provenance can never satisfy the force-ship
    // guard above.
    if matches!(old_phase, Phase::BlockedNeedsUser) && !matches!(to, Phase::BlockedNeedsUser) {
        state.block_origin = None;
    }

    state
        .log_event(format!(
            "transitioned phase from {:?} to {:?}",
            old_phase, to
        ))
        .context("failed to log transition event")?;

    state
        .save()
        .context("failed to save state after transition")?;

    Ok(())
}

/// Set `block_origin` and transition to `BlockedNeedsUser`. The single throat
/// for entering a block — every code path that would have called
/// `execute_transition(state, Phase::BlockedNeedsUser)` should call this
/// instead so the persisted provenance is always populated.
pub fn block_with_origin(state: &mut SessionState, origin: BlockOrigin) -> Result<()> {
    state.block_origin = Some(origin);
    execute_transition(state, Phase::BlockedNeedsUser)
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
    let target = Phase::FinalValidation(round);
    // Validate before incrementing so an illegal source phase cannot leak a
    // stale attempt count into the persisted state.
    validate_transition(&state.current_phase, &target).map_err(|e| anyhow::anyhow!("{e}"))?;
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
    let target = Phase::Simplification(round);
    // Validate before incrementing so an illegal source phase cannot leak a
    // stale attempt count into the persisted state.
    validate_transition(&state.current_phase, &target).map_err(|e| anyhow::anyhow!("{e}"))?;
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
    match stage {
        "brainstorm" => vec![
            SectionPart::Brainstorm,
            SectionPart::Stage(stage.to_string()),
        ],
        "spec-review" => vec![
            SectionPart::SpecReview,
            SectionPart::Stage(stage.to_string()),
        ],
        "planning" => vec![SectionPart::Planning, SectionPart::Stage(stage.to_string())],
        "plan-review" if matches!(state.current_phase, Phase::BuilderRecoveryPlanReview(_)) => {
            vec![
                SectionPart::Iteration(recovery_iteration_for_path(state, task_id)),
                SectionPart::RecoveryPlanReview { round },
                SectionPart::Stage(stage.to_string()),
            ]
        }
        "plan-review" => vec![
            SectionPart::PlanReview,
            SectionPart::Stage(stage.to_string()),
        ],
        "sharding" if matches!(state.current_phase, Phase::BuilderRecoverySharding(_)) => {
            vec![
                SectionPart::Iteration(recovery_iteration_for_path(state, task_id)),
                SectionPart::RecoverySharding { round },
                SectionPart::Stage(stage.to_string()),
            ]
        }
        "sharding" => vec![SectionPart::Sharding, SectionPart::Stage(stage.to_string())],
        "recovery" => vec![
            SectionPart::Iteration(recovery_iteration_for_path(state, task_id)),
            SectionPart::Recovery { round },
            SectionPart::Stage(stage.to_string()),
        ],
        "simplifier" => vec![
            SectionPart::Iteration(loop_iteration_for_round(state, round)),
            SectionPart::Simplification,
            SectionPart::Round { n: round, attempt },
            SectionPart::Stage(stage.to_string()),
        ],
        "final-validation" => vec![
            SectionPart::Iteration(loop_iteration_for_round(state, round)),
            SectionPart::FinalValidation,
            SectionPart::Round { n: round, attempt },
            SectionPart::Stage(stage.to_string()),
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
            let mut path = vec![SectionPart::Iteration(iteration), SectionPart::Loop];
            if let Some(tid) = task_id {
                path.push(SectionPart::Task(tid));
            }
            path.push(SectionPart::Round { n: round, attempt });
            path.push(SectionPart::Stage(stage.to_string()));
            path
        }
        _ => vec![SectionPart::Stage(stage.to_string())],
    }
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
pub fn start_agent_run(
    state: &mut SessionState,
    stage: String,
    task_id: Option<u32>,
    round: u32,
    attempt: u32,
    model: String,
    vendor: String,
    window_name: String,
    effort: EffortLevel,
    modes: LaunchModes,
) -> u64 {
    let path = compute_section_path(state, &stage, task_id, round, attempt);
    state.create_run_record(
        stage,
        task_id,
        round,
        attempt,
        model,
        vendor,
        window_name,
        effort,
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
        vendor: run.vendor.clone(),
        unverified,
        error,
    })
}

pub fn resume_running_runs(state: &mut SessionState) -> Result<Option<u64>> {
    state.resume_running_runs()
}

/// Try to read and parse a TOML artifact at `path`. Returns an error if the
/// file is missing or malformed — the orchestrator treats either case as an
/// incomplete agent turn and retries.
pub fn try_parse_toml_artifact<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("artifact missing or unreadable: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("unparseable TOML artifact: {}", path.display()))
}

#[cfg(test)]
mod section_path_capture_tests {
    use super::*;
    use crate::adapters::EffortLevel;
    use crate::logic::pipeline::phase::Phase;
    use crate::logic::pipeline::state::{
        LaunchModes, PipelineItem, PipelineItemStatus, SectionPart, SessionState,
    };

    #[test]
    fn coder_run_captures_iteration_loop_task_round_stage_path() {
        let mut state = SessionState::new("path-capture".to_string());
        state.current_phase = Phase::ImplementationRound(9);
        state.builder.pipeline_items.push(PipelineItem {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(4),
            round: Some(9),
            status: PipelineItemStatus::Running,
            title: Some("Extract UI".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        let id = start_agent_run(
            &mut state,
            "coder".to_string(),
            Some(4),
            9,
            1,
            "claude-opus-4.7".to_string(),
            "claude".to_string(),
            "[Round 9 Coder]".to_string(),
            EffortLevel::Tough,
            LaunchModes::default(),
        );
        let run = state.agent_runs.iter().find(|r| r.id == id).expect("run");
        assert_eq!(
            run.section_path.as_deref(),
            Some(
                &[
                    SectionPart::Iteration(1),
                    SectionPart::Loop,
                    SectionPart::Task(4),
                    SectionPart::Round { n: 9, attempt: 1 },
                    SectionPart::Stage("coder".to_string()),
                ][..]
            )
        );
    }

    #[test]
    fn simplifier_run_captures_iteration_simplification_round_stage_path() {
        let mut state = SessionState::new("simpl-capture".to_string());
        state.current_phase = Phase::Simplification(9);
        state.builder.pipeline_items.push(PipelineItem {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(9),
            status: PipelineItemStatus::Approved,
            title: Some("Some task".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        let id = start_agent_run(
            &mut state,
            "simplifier".to_string(),
            None,
            9,
            2,
            "claude-opus-4-6".to_string(),
            "claude".to_string(),
            "[Simplifier]".to_string(),
            EffortLevel::Normal,
            LaunchModes::default(),
        );
        let run = state.agent_runs.iter().find(|r| r.id == id).expect("run");
        assert_eq!(
            run.section_path.as_deref(),
            Some(
                &[
                    SectionPart::Iteration(1),
                    SectionPart::Simplification,
                    SectionPart::Round { n: 9, attempt: 2 },
                    SectionPart::Stage("simplifier".to_string()),
                ][..]
            )
        );
    }

    #[test]
    fn brainstorm_run_captures_brainstorm_stage_path() {
        let mut state = SessionState::new("brainstorm".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let id = start_agent_run(
            &mut state,
            "brainstorm".to_string(),
            None,
            0,
            1,
            "x".to_string(),
            "y".to_string(),
            "[Brainstorm]".to_string(),
            EffortLevel::Normal,
            LaunchModes::default(),
        );
        let run = state.agent_runs.iter().find(|r| r.id == id).expect("run");
        assert_eq!(
            run.section_path.as_deref(),
            Some(
                &[
                    SectionPart::Brainstorm,
                    SectionPart::Stage("brainstorm".to_string()),
                ][..]
            )
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_parse_toml_artifact_missing_file() {
        let result = try_parse_toml_artifact::<toml::Value>(Path::new("/nonexistent/path.toml"));
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("missing or unreadable"));
    }

    #[test]
    fn test_try_parse_toml_artifact_malformed() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not { valid toml").unwrap();
        let result = try_parse_toml_artifact::<toml::Value>(&path);
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("unparseable TOML"));
    }

    #[test]
    fn test_try_parse_toml_artifact_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("ok.toml");
        std::fs::write(&path, "status = \"approved\"\nsummary = \"good\"").unwrap();
        let val: toml::Value = try_parse_toml_artifact(&path).unwrap();
        assert_eq!(val.get("status").and_then(|v| v.as_str()), Some("approved"));
        assert_eq!(val.get("summary").and_then(|v| v.as_str()), Some("good"));
    }

    /// Run `f` with a private `CODEXIZE_ROOT` so `execute_transition`'s
    /// implicit `SessionState::save` writes into a temp directory that gets
    /// cleaned up.
    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::logic::pipeline::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("CODEXIZE_ROOT");
        // SAFETY: `set_var`/`remove_var` are not thread-safe on *nix; the
        // `test_fs_lock` mutex serializes every test that touches the env.
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
                None => std::env::remove_var("CODEXIZE_ROOT"),
            }
        }
        result.unwrap()
    }

    #[test]
    fn force_ship_rejected_without_final_validation_origin() {
        with_temp_root(|| {
            let mut state = SessionState::new("force-ship-recovery".to_string());
            state.current_phase = Phase::BlockedNeedsUser;
            state.block_origin = Some(BlockOrigin::BuilderRecovery);
            let err =
                execute_transition(&mut state, Phase::Done).expect_err("expected guard failure");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("force-ship"),
                "guard error must mention force-ship: {msg}"
            );
            assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
        });
    }

    #[test]
    fn force_ship_rejected_when_block_origin_missing() {
        with_temp_root(|| {
            let mut state = SessionState::new("force-ship-missing".to_string());
            state.current_phase = Phase::BlockedNeedsUser;
            state.block_origin = None;
            let err =
                execute_transition(&mut state, Phase::Done).expect_err("expected guard failure");
            assert!(format!("{err:#}").contains("force-ship"));
        });
    }

    #[test]
    fn force_ship_allowed_with_final_validation_origin() {
        with_temp_root(|| {
            let mut state = SessionState::new("force-ship-ok".to_string());
            state.current_phase = Phase::BlockedNeedsUser;
            state.block_origin = Some(BlockOrigin::FinalValidation);
            execute_transition(&mut state, Phase::Done).expect("force-ship must succeed");
            assert_eq!(state.current_phase, Phase::Done);
            assert!(state.block_origin.is_none());
        });
    }

    #[test]
    fn block_with_origin_sets_field_and_transitions() {
        with_temp_root(|| {
            let mut state = SessionState::new("block-helper".to_string());
            state.current_phase = Phase::PlanReviewRunning;
            block_with_origin(&mut state, BlockOrigin::PlanReview)
                .expect("block transition succeeds");
            assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
            assert_eq!(state.block_origin, Some(BlockOrigin::PlanReview));
        });
    }

    #[test]
    fn leaving_block_clears_origin() {
        with_temp_root(|| {
            let mut state = SessionState::new("leave-block".to_string());
            state.current_phase = Phase::BlockedNeedsUser;
            state.block_origin = Some(BlockOrigin::Brainstorm);
            execute_transition(&mut state, Phase::BrainstormRunning).expect("rewind succeeds");
            assert_eq!(state.current_phase, Phase::BrainstormRunning);
            assert!(state.block_origin.is_none(), "origin must clear on leave");
        });
    }

    #[test]
    fn final_validation_round_trip_through_execute_transition() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-round-trip".to_string());
            state.current_phase = Phase::ReviewRound(2);
            execute_transition(&mut state, Phase::Simplification(2)).unwrap();
            execute_transition(&mut state, Phase::FinalValidation(2)).unwrap();
            assert_eq!(state.current_phase, Phase::FinalValidation(2));
            execute_transition(&mut state, Phase::Done).unwrap();
            assert_eq!(state.current_phase, Phase::Done);
        });
    }

    #[test]
    fn enter_final_validation_increments_attempts_for_first_three_entries() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-cap-increment".to_string());
            assert_eq!(state.validation_attempts, 0);

            state.current_phase = Phase::ReviewRound(1);
            execute_transition(&mut state, Phase::Simplification(1)).unwrap();
            let outcome = enter_final_validation(&mut state, 1).unwrap();
            assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 1 });
            assert_eq!(state.current_phase, Phase::FinalValidation(1));
            assert_eq!(state.validation_attempts, 1);

            execute_transition(&mut state, Phase::ImplementationRound(2)).unwrap();
            execute_transition(&mut state, Phase::ReviewRound(2)).unwrap();
            execute_transition(&mut state, Phase::Simplification(2)).unwrap();
            let outcome = enter_final_validation(&mut state, 2).unwrap();
            assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 2 });
            assert_eq!(state.validation_attempts, 2);

            execute_transition(&mut state, Phase::ImplementationRound(3)).unwrap();
            execute_transition(&mut state, Phase::ReviewRound(3)).unwrap();
            execute_transition(&mut state, Phase::Simplification(3)).unwrap();
            let outcome = enter_final_validation(&mut state, 3).unwrap();
            assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 3 });
            assert_eq!(state.validation_attempts, 3);
            assert_eq!(state.current_phase, Phase::FinalValidation(3));
        });
    }

    #[test]
    fn enter_final_validation_caps_fourth_entry_into_blocked() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-cap-block".to_string());
            state.validation_attempts = VALIDATION_ATTEMPT_CAP;
            state.current_phase = Phase::Simplification(4);

            let outcome = enter_final_validation(&mut state, 4).unwrap();

            assert_eq!(outcome, FinalValidationEntry::CapExceeded);
            assert_eq!(state.validation_attempts, VALIDATION_ATTEMPT_CAP);
            assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
            assert_eq!(state.block_origin, Some(BlockOrigin::FinalValidation));
        });
    }

    #[test]
    fn block_origin_simplification_does_not_unlock_force_ship() {
        with_temp_root(|| {
            let mut state = SessionState::new("force-ship-simplification".to_string());
            state.current_phase = Phase::BlockedNeedsUser;
            state.block_origin = Some(BlockOrigin::Simplification);
            let err =
                execute_transition(&mut state, Phase::Done).expect_err("expected guard failure");
            assert!(format!("{err:#}").contains("force-ship"));
            assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
        });
    }

    #[test]
    fn enter_simplification_increments_per_round_counter() {
        with_temp_root(|| {
            let mut state = SessionState::new("simplify-counter".to_string());
            state.current_phase = Phase::ReviewRound(1);
            let outcome = enter_simplification(&mut state, 1).unwrap();
            assert_eq!(outcome, SimplificationEntry::Entered { attempt: 1 });
            assert_eq!(state.current_phase, Phase::Simplification(1));
            assert_eq!(state.simplification_attempts.get(&1).copied(), Some(1));

            execute_transition(&mut state, Phase::ReviewRound(1)).unwrap();
            let outcome = enter_simplification(&mut state, 1).unwrap();
            assert_eq!(outcome, SimplificationEntry::Entered { attempt: 2 });
            assert_eq!(state.simplification_attempts.get(&1).copied(), Some(2));

            execute_transition(&mut state, Phase::FinalValidation(1)).unwrap();
            execute_transition(&mut state, Phase::ImplementationRound(2)).unwrap();
            execute_transition(&mut state, Phase::ReviewRound(2)).unwrap();
            let outcome = enter_simplification(&mut state, 2).unwrap();
            assert_eq!(outcome, SimplificationEntry::Entered { attempt: 1 });
            assert_eq!(state.simplification_attempts.get(&2).copied(), Some(1));
        });
    }

    #[test]
    fn enter_simplification_caps_fourth_entry_into_blocked() {
        with_temp_root(|| {
            let mut state = SessionState::new("simplify-cap".to_string());
            state
                .simplification_attempts
                .insert(4, SIMPLIFICATION_ATTEMPT_CAP);
            state.current_phase = Phase::ReviewRound(4);

            let outcome = enter_simplification(&mut state, 4).unwrap();

            assert_eq!(outcome, SimplificationEntry::CapExceeded);
            assert_eq!(
                state.simplification_attempts.get(&4).copied(),
                Some(SIMPLIFICATION_ATTEMPT_CAP)
            );
            assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
            assert_eq!(state.block_origin, Some(BlockOrigin::Simplification));
        });
    }

    #[test]
    fn enter_simplification_rejects_illegal_source_phase() {
        with_temp_root(|| {
            let mut state = SessionState::new("simplify-illegal-source".to_string());
            state.current_phase = Phase::PlanningRunning;

            let err = enter_simplification(&mut state, 1).expect_err("must reject");
            assert!(format!("{err:#}").contains("Cannot transition"));
            assert!(!state.simplification_attempts.contains_key(&1));
            assert_eq!(state.current_phase, Phase::PlanningRunning);
        });
    }

    #[test]
    fn enter_final_validation_rejects_illegal_source_phase() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-illegal-source".to_string());
            state.current_phase = Phase::IdeaInput;

            let err = enter_final_validation(&mut state, 1).expect_err("must reject");
            assert!(format!("{err:#}").contains("Cannot transition"));
            assert_eq!(state.validation_attempts, 0);
            assert_eq!(state.current_phase, Phase::IdeaInput);
        });
    }
}
