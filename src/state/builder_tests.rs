use super::*;
use crate::state::PipelineItemStatus;

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
            iteration: 1,
        });
        builder.task_titles.insert(tid, format!("Task {tid}"));
    }
    builder
}

#[test]
fn pipeline_items_deserialize_from_current_shape() {
    let builder: BuilderState = toml::from_str(
        r#"
[[pipeline_items]]
id = 1
stage = "coder"
task_id = 1
status = "approved"
iteration = 1

[[pipeline_items]]
id = 2
stage = "coder"
task_id = 2
round = 5
status = "running"
iteration = 1

[[pipeline_items]]
id = 3
stage = "coder"
task_id = 3
status = "pending"
iteration = 1
"#,
    )
    .expect("builder state should deserialize");

    assert_eq!(builder.done_task_ids(), vec![1]);
    assert_eq!(builder.current_task_id(), Some(2));
    assert_eq!(builder.pending_task_ids(), vec![3]);
}

#[test]
fn stale_task_items_do_not_count_as_blocking_or_selectable() {
    let mut builder = make_builder_with_tasks(&[1, 2, 3]);
    builder.pipeline_items[0].status = PipelineItemStatus::Done;
    builder.pipeline_items[1].status = PipelineItemStatus::Stale;
    builder.pipeline_items[2].status = PipelineItemStatus::Stale;

    assert_eq!(builder.done_task_ids(), vec![1]);
    assert_eq!(builder.current_task_id(), None);
    assert!(builder.pending_task_ids().is_empty());
    assert!(!builder.has_unfinished_tasks());
}

#[test]
fn initializing_new_task_pipeline_marks_prior_unfinished_tasks_stale() {
    let mut state = crate::state::SessionState::new("test-session".to_string());
    state.builder = make_builder_with_tasks(&[1, 2, 3]);
    state.builder.pipeline_items[0].status = PipelineItemStatus::Done;
    state.builder.pipeline_items[1].status = PipelineItemStatus::Running;
    state.builder.pipeline_items[2].status = PipelineItemStatus::HumanBlocked;

    crate::logic::pipeline::initialize_task_pipeline(
        &mut state,
        vec![(10, "New task".to_string())],
    );

    let stale_ids = state
        .builder
        .pipeline_items
        .iter()
        .filter(|item| item.status == PipelineItemStatus::Stale)
        .filter_map(|item| item.task_id)
        .collect::<Vec<_>>();
    assert_eq!(stale_ids, vec![2, 3]);
    assert_eq!(state.builder.pending_task_ids(), vec![10]);
    assert_eq!(state.builder.done_task_ids(), vec![1]);
}

#[test]
fn max_task_id_scans_pipeline_titles_and_recovery_sources() {
    let mut builder = make_builder_with_tasks(&[3]);
    builder.task_titles.insert(7, "t7".to_string());
    builder.recovery_prev_max_task_id = Some(5);
    assert_eq!(builder.max_task_id(), 7);
}

#[test]
fn revise_inserts_replacements_and_renumbers_pending_tail() {
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

    assert_eq!(new_ids, vec![5, 6]);
    let task_ids = builder
        .pipeline_items
        .iter()
        .filter(|i| i.stage == "coder")
        .map(|i| i.task_id)
        .collect::<Vec<_>>();
    assert_eq!(
        task_ids,
        vec![Some(1), Some(2), Some(5), Some(6), Some(7), Some(8)]
    );
    assert_eq!(builder.pipeline_items[1].status, PipelineItemStatus::Revise);
    assert_eq!(
        builder.pipeline_items[1].mode.as_deref(),
        Some("superseded")
    );
    assert_eq!(builder.last_verdict.as_deref(), Some("revise"));
}

#[test]
fn revise_preserves_untyped_pending_coder_items() {
    let mut builder = make_builder_with_tasks(&[1, 2, 3]);
    builder.pipeline_items[1].status = PipelineItemStatus::Running;
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
        iteration: 1,
    });

    let ids =
        builder.apply_revise_with_new_tasks(2, vec![("New".into(), "d".into(), "t".into(), 1000)]);

    assert_eq!(ids.len(), 1);
    assert!(builder.pipeline_items.iter().any(|item| {
        item.stage == "coder" && item.title.as_deref() == Some("draft") && item.task_id.is_none()
    }));
}
