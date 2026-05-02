//! Intent-named mutation helpers for live session state.
//!
//! `execute_transition` is the only helper in this module that validates,
//! logs, and persists internally with `SessionState::save`. The remaining
//! helpers perform in-memory mutations only; their callers own persistence so
//! they can batch related state changes into a single save at the workflow
//! boundary.

use super::{
    BlockOrigin, BuilderState, LaunchModes, Modes, PendingGuardDecision, Phase, PipelineItem,
    PipelineItemStatus, RunStatus, SessionState,
};
use crate::adapters::EffortLevel;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::{collections::BTreeSet, path::Path};

/// Errors that can occur during phase transitions.
#[derive(Debug)]
pub enum TransitionError {
    InvalidTransition {
        from: Phase,
        to: Phase,
        reason: String,
    },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionError::InvalidTransition { from, to, reason } => {
                write!(
                    f,
                    "Cannot transition from {} to {}: {}",
                    from.display_name(),
                    to.display_name(),
                    reason
                )
            }
        }
    }
}

impl std::error::Error for TransitionError {}

/// Validate that a transition from `from` to `to` is allowed.
pub fn validate_transition(from: &Phase, to: &Phase) -> Result<(), TransitionError> {
    if !from.can_transition_to(to) {
        return Err(TransitionError::InvalidTransition {
            from: *from,
            to: *to,
            reason: format!(
                "Transition from {} to {} is not allowed",
                from.display_name(),
                to.display_name()
            ),
        });
    }
    Ok(())
}

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

/// Hard cap on `FinalValidation` runs per session. The 4th attempted entry
/// auto-routes to `BlockedNeedsUser` *before* the validator launches, with
/// `block_origin = FinalValidation` so the operator can force-ship or rewind.
/// Hard-coded per spec §4 — there is no runtime override.
pub const VALIDATION_ATTEMPT_CAP: u32 = 3;

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

pub fn prepare_new_session_for_brainstorm(
    state: &mut SessionState,
    idea: impl Into<String>,
    modes: Modes,
) {
    state.modes = modes;
    state.idea_text = Some(idea.into());
    state.current_phase = Phase::BrainstormRunning;
}

pub fn archive_session(state: &mut SessionState) {
    state.archived = true;
}

pub fn restore_archived_session(state: &mut SessionState) {
    state.archived = false;
}

pub fn record_agent_error(state: &mut SessionState, message: impl Into<String>) {
    state.agent_error = Some(message.into());
}

pub fn clear_agent_error(state: &mut SessionState) {
    state.agent_error = None;
}

pub fn set_yolo_mode(state: &mut SessionState, value: bool) {
    state.modes.yolo = value;
}

pub fn set_cheap_mode(state: &mut SessionState, value: bool) {
    state.modes.cheap = value;
}

pub fn record_brainstorm_launch(
    state: &mut SessionState,
    idea: impl Into<String>,
    model: impl Into<String>,
) {
    state.idea_text = Some(idea.into());
    state.selected_model = Some(model.into());
}

pub fn record_session_title(state: &mut SessionState, title: impl Into<String>) {
    state.title = Some(title.into());
}

pub fn record_skip_to_impl_proposal(
    state: &mut SessionState,
    rationale: impl Into<String>,
    kind: crate::artifacts::SkipToImplKind,
) {
    state.skip_to_impl_rationale = Some(rationale.into());
    state.skip_to_impl_kind = Some(kind);
}

pub fn clear_skip_to_impl_proposal(state: &mut SessionState) {
    state.skip_to_impl_rationale = None;
    state.skip_to_impl_kind = None;
}

pub fn reset_builder_after_rewind(state: &mut SessionState) {
    state.builder = BuilderState::default();
}

pub fn load_task_titles_if_empty(
    state: &mut SessionState,
    titles: impl IntoIterator<Item = (u32, String)>,
) {
    if state.builder.task_titles.is_empty() {
        state.builder.task_titles = titles.into_iter().collect();
    }
}

pub fn initialize_task_pipeline(
    state: &mut SessionState,
    tasks: impl IntoIterator<Item = (u32, String)>,
) {
    let tasks = tasks.into_iter().collect::<Vec<_>>();
    state.builder.task_titles = tasks
        .iter()
        .map(|(id, title)| (*id, title.clone()))
        .collect();
    state
        .builder
        .reset_task_pipeline(tasks.into_iter().map(|(id, title)| (id, Some(title))));
}

pub fn ensure_builder_task_for_round(state: &mut SessionState, round: u32) -> Option<u32> {
    state.builder.ensure_task_for_round(round)
}

pub fn mark_task_status(
    state: &mut SessionState,
    task_id: u32,
    status: PipelineItemStatus,
    round: Option<u32>,
) -> bool {
    state.builder.set_task_status(task_id, status, round)
}

pub fn record_builder_verdict(state: &mut SessionState, verdict: impl Into<String>) {
    state.builder.last_verdict = Some(verdict.into());
}

pub fn mark_current_task_for_recovery(
    state: &mut SessionState,
    triggering_round: u32,
) -> Option<u32> {
    let current_task_id = state.builder.current_task_id()?;
    let status = if state.builder.pipeline_items.is_empty() {
        PipelineItemStatus::Pending
    } else {
        PipelineItemStatus::Failed
    };
    state
        .builder
        .set_task_status(current_task_id, status, Some(triggering_round));
    Some(current_task_id)
}

pub fn append_refine_feedback(
    state: &mut SessionState,
    feedback: impl IntoIterator<Item = String>,
) {
    state.builder.pending_refine_feedback.extend(feedback);
}

pub fn take_pending_refine_feedback(state: &mut SessionState) -> Vec<String> {
    std::mem::take(&mut state.builder.pending_refine_feedback)
}

pub fn apply_revise_with_new_tasks(
    state: &mut SessionState,
    task_id: u32,
    new_tasks: Vec<(String, String, String, u32)>,
) -> Vec<u32> {
    let assigned = state
        .builder
        .apply_revise_with_new_tasks(task_id, new_tasks);
    if let Some(first_inserted) = assigned.first().copied() {
        state.builder.current_task = Some(first_inserted);
        state.builder.sync_legacy_queue_views();
    }
    assigned
}

pub fn queue_recovery_stage(
    state: &mut SessionState,
    round: u32,
    trigger: impl Into<String>,
    interactive: bool,
) {
    let title = if interactive {
        "Human-blocked recovery"
    } else {
        "Agent pivot recovery"
    };
    state.builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "recovery".to_string(),
        task_id: None,
        round: Some(round),
        status: PipelineItemStatus::Running,
        title: Some(title.to_string()),
        mode: None,
        trigger: Some(trigger.into()),
        interactive: Some(interactive),
    });
}

pub fn queue_recovery_plan_review(state: &mut SessionState, round: u32) {
    state.builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "plan-review".to_string(),
        task_id: None,
        round: Some(round),
        status: PipelineItemStatus::Pending,
        title: Some("Recovery plan review".to_string()),
        mode: Some("recovery".to_string()),
        trigger: None,
        interactive: Some(false),
    });
}

pub fn queue_recovery_sharding(state: &mut SessionState, round: u32) {
    state.builder.push_pipeline_item(PipelineItem {
        id: 0,
        stage: "sharding".to_string(),
        task_id: None,
        round: Some(round),
        status: PipelineItemStatus::Pending,
        title: Some("Recovery sharding".to_string()),
        mode: Some("recovery".to_string()),
        trigger: None,
        interactive: Some(false),
    });
}

pub fn mark_latest_pipeline_stage_running(state: &mut SessionState, stage: &str) -> bool {
    mark_latest_pipeline_stage(
        state,
        stage,
        PipelineItemStatus::Pending,
        PipelineItemStatus::Running,
    )
}

pub fn mark_latest_pipeline_stage_done(state: &mut SessionState, stage: &str) -> bool {
    mark_latest_pipeline_stage(
        state,
        stage,
        PipelineItemStatus::Running,
        PipelineItemStatus::Done,
    )
}

fn mark_latest_pipeline_stage(
    state: &mut SessionState,
    stage: &str,
    from: PipelineItemStatus,
    to: PipelineItemStatus,
) -> bool {
    if let Some(item) = state
        .builder
        .pipeline_items
        .iter_mut()
        .rev()
        .find(|item| item.stage == stage && item.status == from)
    {
        item.status = to;
        true
    } else {
        false
    }
}

pub fn replace_recovery_pipeline(
    state: &mut SessionState,
    items: Vec<PipelineItem>,
    task_titles: impl IntoIterator<Item = (u32, String)>,
) {
    for (task_id, title) in task_titles {
        state.builder.task_titles.insert(task_id, title);
    }
    state.builder.pipeline_items = items;
    normalize_pipeline_item_ids(&mut state.builder);
    state.builder.sync_legacy_queue_views();
}

fn normalize_pipeline_item_ids(builder: &mut BuilderState) {
    let mut seen = BTreeSet::new();
    let mut next_id = builder.next_pipeline_id();
    for item in &mut builder.pipeline_items {
        if item.id != 0 && seen.insert(item.id) {
            continue;
        }
        while seen.contains(&next_id) {
            next_id += 1;
        }
        item.id = next_id;
        seen.insert(next_id);
        next_id += 1;
    }
}

pub fn set_retry_reset_run_id_cutoff(state: &mut SessionState, run_id: u64) {
    state.builder.retry_reset_run_id_cutoff = Some(run_id);
}

pub fn set_phase_for_operator_retry(state: &mut SessionState, phase: Phase) {
    state.current_phase = phase;
}

pub fn increment_recovery_cycle_count(state: &mut SessionState) -> u32 {
    state.builder.recovery_cycle_count += 1;
    state.builder.recovery_cycle_count
}

pub fn reset_recovery_cycle_count(state: &mut SessionState) {
    state.builder.recovery_cycle_count = 0;
}

pub fn record_builder_recovery_context(
    state: &mut SessionState,
    trigger_task_id: Option<u32>,
    prev_max: Option<u32>,
    prev_task_ids: Vec<u32>,
    trigger_summary: Option<String>,
) {
    state.builder.recovery_trigger_task_id = trigger_task_id.or(state.builder.current_task_id());
    state.builder.recovery_prev_max_task_id = prev_max;
    state.builder.recovery_prev_task_ids = prev_task_ids;
    state.builder.recovery_trigger_summary = trigger_summary;
}

pub fn clear_builder_recovery_context(state: &mut SessionState) {
    state.builder.recovery_trigger_task_id = None;
    state.builder.recovery_prev_max_task_id = None;
    state.builder.recovery_prev_task_ids.clear();
    state.builder.recovery_trigger_summary = None;
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
    )
}

#[derive(Debug, Clone)]
pub struct FinishedRunRecord {
    pub ended_at: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub attempt: u32,
    pub model: String,
    pub vendor: String,
    pub unverified: bool,
    pub error: Option<String>,
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

pub fn record_pending_guard_decision(state: &mut SessionState, decision: PendingGuardDecision) {
    state.pending_guard_decision = Some(decision);
}

pub fn take_pending_guard_decision(
    state: &mut SessionState,
    context: &str,
) -> Result<PendingGuardDecision> {
    state
        .pending_guard_decision
        .take()
        .ok_or_else(|| anyhow::anyhow!("{context}: no pending guard decision"))
}

pub fn clear_pending_guard_decision(state: &mut SessionState) {
    state.pending_guard_decision = None;
}

pub fn restore_guard_originating_phase(state: &mut SessionState, originating: Phase) {
    // The guard modal is an interstitial persisted phase; on "keep", finalization
    // must resume the original running phase before applying its normal successor.
    // Phase::can_transition_to intentionally does not list GitGuardPending back
    // to the paused running phases, so this restore cannot use execute_transition.
    state.current_phase = originating;
}

pub fn resume_running_runs(state: &mut SessionState) -> Result<Option<u64>> {
    state.resume_running_runs()
}

/// Per-stage definition of which artifacts are passed by pointer and which are
/// expected as output. Used by the orchestrator to validate agent runs.
#[derive(Debug, Clone)]
pub struct StageIO {
    pub stage: &'static str,
    pub pointer_artifacts: &'static [&'static str],
    pub writes: &'static [&'static str],
}

pub const BRAINSTORM_IO: StageIO = StageIO {
    stage: "brainstorm",
    pointer_artifacts: &["artifacts/live_summary.txt"],
    writes: &["artifacts/spec.md"],
};

pub const SPEC_REVIEWER_IO: StageIO = StageIO {
    stage: "spec-reviewer",
    pointer_artifacts: &["artifacts/spec.md", "artifacts/live_summary.txt"],
    writes: &["artifacts/spec_review.toml"],
};

pub const PLANNER_IO: StageIO = StageIO {
    stage: "planner",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/spec_review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/plan.md"],
};

pub const PLAN_REVIEWER_IO: StageIO = StageIO {
    stage: "plan-reviewer",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/plan_review.toml"],
};

pub const SHARDER_IO: StageIO = StageIO {
    stage: "sharder",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/tasks.toml"],
};

pub const CODER_IO: StageIO = StageIO {
    stage: "coder",
    pointer_artifacts: &[
        "rounds/{round}/task.toml",
        "artifacts/spec.md",
        "artifacts/plan.md",
        "rounds/{round}/review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["rounds/{round}/coder_summary.toml"],
};

pub const REVIEWER_IO: StageIO = StageIO {
    stage: "reviewer",
    pointer_artifacts: &[
        "rounds/{round}/task.toml",
        "rounds/{round}/review_scope.toml",
        "rounds/{round}/coder_summary.toml",
        "artifacts/spec.md",
        "artifacts/plan.md",
        "rounds/*/review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["rounds/{round}/review.toml"],
};

pub const RECOVERY_IO: StageIO = StageIO {
    stage: "recovery",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/tasks.toml",
        "rounds/{round}/review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/tasks.toml",
        "rounds/{round}/recovery.toml",
    ],
};

/// Recovery-mode plan review: verifies the recovered spec/plan addresses the
/// triggering review before sharding runs.
pub const RECOVERY_PLAN_REVIEWER_IO: StageIO = StageIO {
    stage: "plan-reviewer",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "rounds/{round}/review.toml",
        "rounds/{round}/recovery.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/plan_review.toml"],
};

/// Recovery-mode sharding: regenerates the task queue from the recovered
/// spec/plan while preserving completed task history.
pub const RECOVERY_SHARDER_IO: StageIO = StageIO {
    stage: "sharder",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/tasks.toml"],
};

pub fn stage_io(stage: &str) -> Option<&'static StageIO> {
    stage_io_with_mode(stage, None)
}

/// Lookup StageIO by stage name and optional mode.  The `"recovery"` mode
/// selects the recovery-specific variants for `plan-reviewer` and `sharder`.
pub fn stage_io_with_mode(stage: &str, mode: Option<&str>) -> Option<&'static StageIO> {
    match (stage, mode) {
        ("plan-reviewer", Some("recovery")) => Some(&RECOVERY_PLAN_REVIEWER_IO),
        ("sharder", Some("recovery")) => Some(&RECOVERY_SHARDER_IO),
        ("brainstorm", _) => Some(&BRAINSTORM_IO),
        ("spec-reviewer", _) => Some(&SPEC_REVIEWER_IO),
        ("planner", _) => Some(&PLANNER_IO),
        ("plan-reviewer", _) => Some(&PLAN_REVIEWER_IO),
        ("sharder", _) => Some(&SHARDER_IO),
        ("coder", _) => Some(&CODER_IO),
        ("reviewer", _) => Some(&REVIEWER_IO),
        ("recovery", _) => Some(&RECOVERY_IO),
        _ => None,
    }
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
mod tests {
    use super::*;
    use crate::state::{BuilderState, PipelineItem, PipelineItemStatus};

    #[test]
    fn test_stage_io_lookup() {
        assert!(stage_io("brainstorm").is_some());
        assert!(stage_io("coder").is_some());
        assert!(stage_io("reviewer").is_some());
        assert!(stage_io("recovery").is_some());
        assert!(stage_io("nonexistent").is_none());
    }

    #[test]
    fn test_brainstorm_io_writes_spec() {
        let io = stage_io("brainstorm").unwrap();
        assert!(io.writes.contains(&"artifacts/spec.md"));
    }

    #[test]
    fn test_sharder_io_reads_spec_and_plan() {
        let io = stage_io("sharder").unwrap();
        assert!(io.pointer_artifacts.contains(&"artifacts/spec.md"));
        assert!(io.pointer_artifacts.contains(&"artifacts/plan.md"));
    }

    #[test]
    fn test_coder_io_uses_round_task_artifacts() {
        let io = stage_io("coder").unwrap();
        assert!(io.pointer_artifacts.contains(&"rounds/{round}/task.toml"));
        assert!(io.pointer_artifacts.contains(&"rounds/{round}/review.toml"));
        assert!(io.writes.contains(&"rounds/{round}/coder_summary.toml"));
    }

    #[test]
    fn test_reviewer_io_writes_round_review() {
        let io = stage_io("reviewer").unwrap();
        assert!(io.pointer_artifacts.contains(&"rounds/{round}/task.toml"));
        assert!(
            io.pointer_artifacts
                .contains(&"rounds/{round}/review_scope.toml")
        );
        assert!(
            io.pointer_artifacts
                .contains(&"rounds/{round}/coder_summary.toml")
        );
        assert!(io.writes.contains(&"rounds/{round}/review.toml"));
    }

    #[test]
    fn test_recovery_io_uses_trigger_review_and_writes_recovery() {
        let io = stage_io("recovery").unwrap();
        assert!(io.pointer_artifacts.contains(&"rounds/{round}/review.toml"));
        assert!(io.writes.contains(&"artifacts/spec.md"));
        assert!(io.writes.contains(&"artifacts/plan.md"));
        assert!(io.writes.contains(&"artifacts/tasks.toml"));
        assert!(io.writes.contains(&"rounds/{round}/recovery.toml"));
    }

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
        assert_eq!(val.get("status").unwrap().as_str(), Some("approved"));
    }

    #[test]
    fn replace_recovery_pipeline_assigns_missing_pipeline_ids() {
        let mut state = SessionState::new("test".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Approved,
            title: Some("done".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });

        replace_recovery_pipeline(
            &mut state,
            vec![
                PipelineItem {
                    id: 1,
                    stage: "coder".to_string(),
                    task_id: Some(1),
                    round: Some(1),
                    status: PipelineItemStatus::Approved,
                    title: Some("done".to_string()),
                    mode: None,
                    trigger: None,
                    interactive: None,
                },
                PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(2),
                    round: None,
                    status: PipelineItemStatus::Pending,
                    title: Some("new".to_string()),
                    mode: None,
                    trigger: None,
                    interactive: None,
                },
            ],
            [(2, "new".to_string())],
        );

        let ids = state
            .builder
            .pipeline_items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| *id != 0));
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn test_max_task_id_empty() {
        let builder = BuilderState::default();
        assert_eq!(builder.max_task_id(), 0);
    }

    #[test]
    fn test_max_task_id_from_pipeline() {
        let mut builder = BuilderState::default();
        builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(5),
            round: None,
            status: PipelineItemStatus::Pending,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
        });
        assert_eq!(builder.max_task_id(), 5);
    }

    #[test]
    fn test_max_task_id_from_recovery_snapshot() {
        let builder = BuilderState {
            recovery_prev_max_task_id: Some(10),
            recovery_prev_task_ids: vec![1, 2, 10],
            ..Default::default()
        };
        assert_eq!(builder.max_task_id(), 10);
    }

    #[test]
    fn test_max_task_id_across_all_sources() {
        let mut builder = BuilderState::default();
        builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(3),
            round: None,
            status: PipelineItemStatus::Pending,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
        });
        builder.done = vec![1, 2];
        builder.task_titles.insert(7, "t7".to_string());
        builder.recovery_prev_max_task_id = Some(5);
        assert_eq!(builder.max_task_id(), 7);
    }

    fn make_builder_with_tasks(task_ids: &[u32]) -> BuilderState {
        let mut builder = BuilderState::default();
        for &tid in task_ids {
            builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "coder".to_string(),
                task_id: Some(tid),
                round: None,
                status: PipelineItemStatus::Pending,
                title: Some(format!("Task {tid}")),
                mode: None,
                trigger: None,
                interactive: None,
            });
            builder.task_titles.insert(tid, format!("Task {tid}"));
        }
        builder
    }

    #[test]
    fn test_apply_revise_basic_insertion() {
        let mut builder = make_builder_with_tasks(&[1, 2, 3, 4]);
        builder.pipeline_items[0].status = PipelineItemStatus::Approved;
        builder.pipeline_items[1].status = PipelineItemStatus::Running;

        let new_ids = builder.apply_revise_with_new_tasks(
            2,
            vec![
                ("Split A".into(), "desc".into(), "test".into(), 1000),
                ("Split B".into(), "desc".into(), "test".into(), 1000),
            ],
        );

        assert_eq!(new_ids.len(), 2);
        assert_eq!(new_ids[0], 5);
        assert_eq!(new_ids[1], 6);

        let task_ids: Vec<Option<u32>> = builder
            .pipeline_items
            .iter()
            .filter(|i| i.stage == "coder")
            .map(|i| i.task_id)
            .collect();
        // [1(approved), 2(revise), 5(pending), 6(pending), 7(pending=old3), 8(pending=old4)]
        assert_eq!(task_ids.len(), 6);
        assert_eq!(task_ids[0], Some(1));
        assert_eq!(task_ids[1], Some(2));
        assert_eq!(task_ids[2], Some(5));
        assert_eq!(task_ids[3], Some(6));
        assert_eq!(task_ids[4], Some(7));
        assert_eq!(task_ids[5], Some(8));
    }

    #[test]
    fn test_apply_revise_renumbers_only_pending() {
        let mut builder = make_builder_with_tasks(&[1, 2, 3, 4]);
        builder.pipeline_items[0].status = PipelineItemStatus::Approved;
        builder.pipeline_items[1].status = PipelineItemStatus::Running;

        let _ids = builder
            .apply_revise_with_new_tasks(2, vec![("New".into(), "d".into(), "t".into(), 1000)]);

        // Task 1 (approved) stays as 1
        assert_eq!(builder.pipeline_items[0].task_id, Some(1));
        assert_eq!(
            builder.pipeline_items[0].status,
            PipelineItemStatus::Approved
        );
        // Task 2 (current) marked as revise
        assert_eq!(builder.pipeline_items[1].task_id, Some(2));
        assert_eq!(builder.pipeline_items[1].status, PipelineItemStatus::Revise);
    }

    #[test]
    fn test_apply_revise_monotonic_across_recovery() {
        let mut builder = make_builder_with_tasks(&[1, 2, 3]);
        builder.recovery_prev_max_task_id = Some(10);
        builder.pipeline_items[0].status = PipelineItemStatus::Approved;
        builder.pipeline_items[1].status = PipelineItemStatus::Running;

        let ids = builder
            .apply_revise_with_new_tasks(2, vec![("New".into(), "d".into(), "t".into(), 1000)]);

        assert_eq!(ids[0], 11);
    }

    #[test]
    fn test_apply_revise_updates_task_titles() {
        let mut builder = make_builder_with_tasks(&[1, 2, 3]);
        builder.pipeline_items[1].status = PipelineItemStatus::Running;

        let ids = builder.apply_revise_with_new_tasks(
            2,
            vec![("Replacement".into(), "d".into(), "t".into(), 1000)],
        );

        assert_eq!(
            builder.task_titles.get(&ids[0]).map(|s| s.as_str()),
            Some("Replacement")
        );
        // Old task 3 was renumbered to 4; its title should follow
        let new_id_for_old_3 = ids[0] + 1;
        assert_eq!(
            builder
                .task_titles
                .get(&new_id_for_old_3)
                .map(|s| s.as_str()),
            Some("Task 3")
        );
        assert!(!builder.task_titles.contains_key(&3));
    }

    #[test]
    fn test_apply_revise_empty_new_tasks_is_noop() {
        let mut builder = make_builder_with_tasks(&[1, 2]);
        let ids = builder.apply_revise_with_new_tasks(1, vec![]);
        assert!(ids.is_empty());
        assert_eq!(builder.pipeline_items.len(), 2);
    }

    #[test]
    fn test_apply_revise_syncs_legacy_views() {
        let mut builder = make_builder_with_tasks(&[1, 2, 3]);
        builder.pipeline_items[0].status = PipelineItemStatus::Approved;
        builder.pipeline_items[1].status = PipelineItemStatus::Running;

        builder.apply_revise_with_new_tasks(2, vec![("New".into(), "d".into(), "t".into(), 1000)]);

        assert!(builder.done.contains(&1));
        assert!(builder.pending.len() >= 2);
        assert_eq!(builder.last_verdict.as_deref(), Some("revise"));
    }

    #[test]
    fn test_apply_revise_skips_pending_coder_with_no_task_id() {
        let mut builder = make_builder_with_tasks(&[1, 2, 3]);
        builder.pipeline_items[1].status = PipelineItemStatus::Running;
        // Inject a pending coder item with no task_id after the running task
        // to exercise the renumber loop's None branch.
        builder.pipeline_items.push(PipelineItem {
            id: builder.next_pipeline_id(),
            stage: "coder".to_string(),
            task_id: None,
            round: None,
            status: PipelineItemStatus::Pending,
            title: Some("draft".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });

        let ids = builder
            .apply_revise_with_new_tasks(2, vec![("New".into(), "d".into(), "t".into(), 1000)]);

        assert_eq!(ids.len(), 1);
        let untyped_still_none = builder.pipeline_items.iter().any(|item| {
            item.stage == "coder"
                && item.title.as_deref() == Some("draft")
                && item.task_id.is_none()
        });
        assert!(
            untyped_still_none,
            "no-task-id coder pending row must be left untouched"
        );
    }

    /// Run `f` with a private `CODEXIZE_ROOT` so `execute_transition`'s
    /// implicit `SessionState::save` writes into a temp directory that gets
    /// cleaned up. Mirrors `state::tests_mod::with_temp_root`; defined here
    /// because `tests_mod` is a sibling module.
    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock()
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
            // Phase must remain unchanged on rejection.
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
            // Block origin is cleared on leaving BlockedNeedsUser so a stale
            // value cannot satisfy a later force-ship guard.
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

            // Attempt 1: ReviewRound(1) -> FinalValidation(1).
            state.current_phase = Phase::ReviewRound(1);
            let outcome = enter_final_validation(&mut state, 1).unwrap();
            assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 1 });
            assert_eq!(state.current_phase, Phase::FinalValidation(1));
            assert_eq!(state.validation_attempts, 1);

            // Attempt 2: simulate goal_gap pivot then re-validation.
            execute_transition(&mut state, Phase::ImplementationRound(2)).unwrap();
            execute_transition(&mut state, Phase::ReviewRound(2)).unwrap();
            let outcome = enter_final_validation(&mut state, 2).unwrap();
            assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 2 });
            assert_eq!(state.validation_attempts, 2);

            // Attempt 3.
            execute_transition(&mut state, Phase::ImplementationRound(3)).unwrap();
            execute_transition(&mut state, Phase::ReviewRound(3)).unwrap();
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
            // Pretend three validation rounds already ran and the coder loop
            // just produced new work for round 4.
            state.validation_attempts = VALIDATION_ATTEMPT_CAP;
            state.current_phase = Phase::ReviewRound(4);

            let outcome = enter_final_validation(&mut state, 4).unwrap();

            assert_eq!(outcome, FinalValidationEntry::CapExceeded);
            // The cap path must not increment past the limit.
            assert_eq!(state.validation_attempts, VALIDATION_ATTEMPT_CAP);
            // It must block with the final-validation origin so force-ship
            // is unlocked.
            assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
            assert_eq!(state.block_origin, Some(BlockOrigin::FinalValidation));
        });
    }

    #[test]
    fn enter_final_validation_rejects_illegal_source_phase() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-illegal-source".to_string());
            state.current_phase = Phase::IdeaInput;

            let err = enter_final_validation(&mut state, 1).expect_err("must reject");
            assert!(format!("{err:#}").contains("Cannot transition"));
            // No half-applied state: counter must not have advanced and the
            // phase must remain unchanged.
            assert_eq!(state.validation_attempts, 0);
            assert_eq!(state.current_phase, Phase::IdeaInput);
        });
    }
}
