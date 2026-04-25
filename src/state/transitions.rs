use super::{Phase, SessionState};
use anyhow::{Context, Result};
use std::path::Path;

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
pub fn execute_transition(state: &mut SessionState, to: Phase) -> Result<()> {
    validate_transition(&state.current_phase, &to).map_err(|e| anyhow::anyhow!("{e}"))?;

    let old_phase = state.current_phase;
    state.current_phase = to;

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
    writes: &[],
};

pub const REVIEWER_IO: StageIO = StageIO {
    stage: "reviewer",
    pointer_artifacts: &[
        "rounds/{round}/task.toml",
        "rounds/{round}/review_scope.toml",
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

pub fn stage_io(stage: &str) -> Option<&'static StageIO> {
    match stage {
        "brainstorm" => Some(&BRAINSTORM_IO),
        "spec-reviewer" => Some(&SPEC_REVIEWER_IO),
        "planner" => Some(&PLANNER_IO),
        "plan-reviewer" => Some(&PLAN_REVIEWER_IO),
        "sharder" => Some(&SHARDER_IO),
        "coder" => Some(&CODER_IO),
        "reviewer" => Some(&REVIEWER_IO),
        "recovery" => Some(&RECOVERY_IO),
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
        assert!(io.writes.is_empty());
    }

    #[test]
    fn test_reviewer_io_writes_round_review() {
        let io = stage_io("reviewer").unwrap();
        assert!(io.pointer_artifacts.contains(&"rounds/{round}/task.toml"));
        assert!(
            io.pointer_artifacts
                .contains(&"rounds/{round}/review_scope.toml")
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
}
