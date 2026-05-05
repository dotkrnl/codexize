//! In-memory pipeline state mutators.
//!
//! Every helper in this module is pure: it mutates `SessionState` (or
//! returns a value derived from it) but performs no IO, no clock reads, and
//! no process spawning. The persisting counterparts that wrap these helpers
//! with logging and `state.save()` live in
//! [`crate::data::persistence::transitions`].

use crate::adapters::EffortLevel;
use crate::logic::pipeline::builder::BuilderState;
use crate::logic::pipeline::phase::Phase;
use crate::logic::pipeline::state::{
    LaunchModes, Modes, PendingGuardDecision, PipelineItem, PipelineItemStatus, SessionState,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

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

/// Hard cap on `FinalValidation` runs per session. The 4th attempted entry
/// auto-routes to `BlockedNeedsUser` *before* the validator launches, with
/// `block_origin = FinalValidation` so the operator can force-ship or rewind.
/// Hard-coded per spec §4 — there is no runtime override.
pub const VALIDATION_ATTEMPT_CAP: u32 = 3;

/// Hard cap on `Simplification(round)` runs for a given round. The 4th
/// attempted entry for the same round auto-routes to `BlockedNeedsUser` with
/// `block_origin = Simplification`. Force-ship is *not* unlocked from a
/// simplification block — that escape hatch remains tied to
/// `BlockOrigin::FinalValidation`.
pub const SIMPLIFICATION_ATTEMPT_CAP: u32 = 3;

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

/// Append validator-emitted gap tasks under a fresh outer iteration so the
/// dashboard can render them in their own (Loop, Simplification,
/// FinalValidation) trio after the prior iteration's FV. Without the bump,
/// the new tasks would land in the same `Loop` subtree as the original
/// tasks and their later-round messages would render *before* the prior
/// FV's messages, breaking the chronology of the message timeline.
///
/// `iteration` is the new outer iteration for these tasks — typically
/// `(max existing pipeline_items.iteration) + 1`. The caller computes it
/// once so all gap tasks emitted by the same FV verdict share an iteration.
pub fn append_final_validation_gap_tasks(
    state: &mut SessionState,
    tasks: impl IntoIterator<Item = (u32, String)>,
    iteration: u32,
) {
    for (task_id, title) in tasks {
        state.builder.task_titles.insert(task_id, title.clone());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(task_id),
            round: None,
            status: PipelineItemStatus::Pending,
            title: Some(title),
            mode: None,
            trigger: None,
            interactive: None,
            iteration,
        });
    }
    state.builder.sync_legacy_queue_views();
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
    let iteration = recovery_iteration(state);
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
        iteration,
    });
}

pub fn queue_recovery_plan_review(state: &mut SessionState, round: u32) {
    let iteration = recovery_iteration(state);
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
        iteration,
    });
}

pub fn queue_recovery_sharding(state: &mut SessionState, round: u32) {
    let iteration = recovery_iteration(state);
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
        iteration,
    });
}

/// Pick the outer iteration that recovery sub-pipeline items should join:
/// the iteration of the task that triggered recovery (so the recovery node
/// renders inside the same Loop[N] subtree as its trigger task), falling
/// back to the latest iteration in the pipeline.
fn recovery_iteration(state: &SessionState) -> u32 {
    let trigger = state.builder.recovery_trigger_task_id;
    if let Some(task_id) = trigger
        && let Some(item) = state
            .builder
            .pipeline_items
            .iter()
            .find(|item| item.stage == "coder" && item.task_id == Some(task_id))
    {
        return item.iteration;
    }
    state
        .builder
        .pipeline_items
        .iter()
        .map(|item| item.iteration)
        .max()
        .unwrap_or(1)
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

/// Behavior-preserving cleanup pass that fires on every normal entry into
/// `FinalValidation`. The simplifier reads spec/plan, the round's review
/// scope (for `base_sha..HEAD`), and the live summary, and writes its
/// verdict to `rounds/{round}/simplification.toml`.
pub const SIMPLIFIER_IO: StageIO = StageIO {
    stage: "simplifier",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "rounds/{round}/review_scope.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["rounds/{round}/simplification.toml"],
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
        ("simplifier", _) => Some(&SIMPLIFIER_IO),
        ("recovery", _) => Some(&RECOVERY_IO),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn create_run_record_in_memory(
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
    started_at: DateTime<Utc>,
    hostname: Option<String>,
    mount_device_id: Option<u64>,
) -> u64 {
    let id = state.next_agent_run_id();
    let run = crate::logic::pipeline::state::RunRecord {
        id,
        stage,
        task_id,
        round,
        attempt,
        model,
        vendor,
        window_name,
        started_at,
        ended_at: None,
        status: crate::logic::pipeline::state::RunStatus::Running,
        error: None,
        effort,
        modes,
        hostname,
        mount_device_id,
    };
    state.agent_runs.push(run);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

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
            iteration: 1,
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
                    iteration: 1,
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
                    iteration: 1,
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
    fn simplifier_io_lookup_and_paths() {
        let io = stage_io("simplifier").expect("simplifier StageIO is registered");
        assert_eq!(io.stage, "simplifier");
        assert!(io.pointer_artifacts.contains(&"artifacts/spec.md"));
        assert!(
            io.pointer_artifacts
                .contains(&"rounds/{round}/review_scope.toml")
        );
        assert!(io.writes.contains(&"rounds/{round}/simplification.toml"));
    }
}
