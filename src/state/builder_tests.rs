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
fn legacy_queue_deserialize_hydrates_pipeline_items() {
    let builder: BuilderState = toml::from_str(
        r#"
pending = [3, 4]
done = [1]
current_task = 2
iteration = 7
"#,
    )
    .expect("legacy builder state should deserialize");

    let task_ids = builder
        .pipeline_items
        .iter()
        .map(|item| (item.task_id, item.status, item.round))
        .collect::<Vec<_>>();
    assert_eq!(
        task_ids,
        vec![
            (Some(1), PipelineItemStatus::Approved, None),
            (Some(2), PipelineItemStatus::Running, Some(7)),
            (Some(3), PipelineItemStatus::Pending, None),
            (Some(4), PipelineItemStatus::Pending, None),
        ]
    );
    assert_eq!(builder.done_task_ids(), vec![1]);
    assert_eq!(builder.current_task_id(), Some(2));
    assert_eq!(builder.pending_task_ids(), vec![3, 4]);
}

#[test]
fn pipeline_items_deserialize_overwrites_stale_legacy_views() {
    let builder: BuilderState = toml::from_str(
        r#"
pending = [99]
done = [98]
current_task = 97

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
