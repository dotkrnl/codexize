use super::*;

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
