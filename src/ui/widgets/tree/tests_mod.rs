use super::*;
use crate::state::PipelineItem;

fn run(id: u64, stage: &str, status: RunStatus) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Test]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

fn collect_run_ids(node: &Node, out: &mut Vec<u64>) {
    if let Some(id) = node.run_id.or(node.leaf_run_id) {
        out.push(id);
    }
    for child in &node.children {
        collect_run_ids(child, out);
    }
}

fn set_builder_tasks(
    state: &mut SessionState,
    done: &[u32],
    current: Option<u32>,
    pending: &[u32],
) {
    state.builder.pipeline_items.clear();
    for task_id in done {
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(*task_id),
            round: None,
            status: PipelineItemStatus::Approved,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
    }
    if let Some(task_id) = current {
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(task_id),
            round: None,
            status: PipelineItemStatus::Running,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
    }
    for task_id in pending {
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(*task_id),
            round: None,
            status: PipelineItemStatus::Pending,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
    }
}

#[test]
fn test_build_tree_single_stage() {
    let mut state = SessionState::new("test".to_string());
    state.agent_runs.push(run(1, "brainstorm", RunStatus::Done));
    let nodes = build_tree(&state);
    assert_eq!(nodes.len(), 10); // Idea + 6 stages + Simplification + Final Validation + Dreaming
    let brainstorm = nodes.iter().find(|n| n.label == "Brainstorm").unwrap();
    assert_eq!(brainstorm.kind, NodeKind::Stage);
    assert_eq!(brainstorm.status, NodeStatus::Done);
}

#[test]
fn test_collapse_preserves_task_node() {
    // Task nodes are never absorbed, ensuring they always remain visible
    let mut stage = Node {
        label: "Loop".to_string(),
        kind: NodeKind::Stage,
        status: NodeStatus::Done,
        summary: "".to_string(),
        children: vec![Node {
            label: "Task 1".to_string(),
            kind: NodeKind::Task,
            status: NodeStatus::Done,
            summary: "".to_string(),
            children: vec![Node {
                label: "Round 1".to_string(),
                kind: NodeKind::Round,
                status: NodeStatus::Done,
                summary: "".to_string(),
                children: vec![Node {
                    label: "Builder".to_string(),
                    kind: NodeKind::AgentRun,
                    status: NodeStatus::Done,
                    summary: "".to_string(),
                    children: vec![],
                    run_id: Some(1),
                    leaf_run_id: None,
                }],
                run_id: None,
                leaf_run_id: None,
            }],
            run_id: None,
            leaf_run_id: None,
        }],
        run_id: None,
        leaf_run_id: None,
    };
    collapse_tree(&mut stage);
    // Task preserved (never absorbed), Round+AgentRun absorbed
    assert_eq!(stage.children.len(), 1);
    assert_eq!(stage.leaf_run_id, None);
    assert_eq!(stage.children[0].label, "Task 1");
    assert_eq!(stage.children[0].leaf_run_id, Some(1));
}

#[test]
fn test_collapse_preserves_multi_child() {
    let mut stage = Node {
        label: "Spec Review".to_string(),
        kind: NodeKind::Stage,
        status: NodeStatus::Done,
        summary: "".to_string(),
        children: vec![
            Node {
                label: "Round 1 · gpt-5".to_string(),
                kind: NodeKind::AgentRun,
                status: NodeStatus::Done,
                summary: "".to_string(),
                children: vec![],
                run_id: Some(1),
                leaf_run_id: None,
            },
            Node {
                label: "Round 2 · o3".to_string(),
                kind: NodeKind::AgentRun,
                status: NodeStatus::Done,
                summary: "".to_string(),
                children: vec![],
                run_id: Some(2),
                leaf_run_id: None,
            },
        ],
        run_id: None,
        leaf_run_id: None,
    };
    collapse_tree(&mut stage);
    assert_eq!(stage.children.len(), 2);
    assert_eq!(stage.leaf_run_id, None);
}

#[test]
fn test_collapse_review_single_round_multiple_attempts() {
    let mut state = SessionState::new("test".to_string());
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Spec Review r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Failed,
        error: Some("quota exceeded".to_string()),
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 2,
        model: "gpt-5".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Spec Review r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    let nodes = build_tree(&state);
    let spec_review = nodes.iter().find(|n| n.label == "Spec Review").unwrap();
    // Round collapsed (only one round) → its AgentRun children hoisted to Stage.
    // AgentRun preserved because two runs.
    assert!(
        spec_review
            .children
            .iter()
            .all(|c| c.kind == NodeKind::AgentRun)
    );
    assert_eq!(spec_review.children.len(), 2);
    assert_eq!(spec_review.status, NodeStatus::Done);
    assert_eq!(spec_review.summary, "spec review complete");
}

#[test]
fn test_retry_success_collapses_to_done_for_simple_stage() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::SpecReviewRunning;
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Planning]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Failed,
        error: Some("quota exceeded".to_string()),
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt: 2,
        model: "gpt-5".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Planning]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    let nodes = build_tree(&state);
    let planning = nodes.iter().find(|n| n.label == "Planning").unwrap();
    assert_eq!(planning.status, NodeStatus::Done);
    assert_eq!(planning.summary, "planning complete");
}

#[test]
fn test_retry_success_collapses_round_status_in_builder() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    set_builder_tasks(&mut state, &[], Some(1), &[]);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Builder t1 r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Failed,
        error: Some("timeout".to_string()),
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 2,
        model: "claude-opus-4-7".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Builder t1 r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    let nodes = build_tree(&state);
    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    // Walk into the task/round tree (collapsing may hoist children).
    let rollup_failed = |n: &Node| {
        fn walk(n: &Node, found: &mut bool) {
            if n.kind != NodeKind::AgentRun && n.status == NodeStatus::Failed {
                *found = true;
            }
            for c in &n.children {
                walk(c, found);
            }
        }
        let mut found = false;
        walk(n, &mut found);
        found
    };
    assert!(
        !rollup_failed(builder),
        "no task/round/stage rollup should be Failed when the latest attempt succeeded"
    );
}

#[test]
fn failed_unverified_run_maps_to_distinct_node_status() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    set_builder_tasks(&mut state, &[], Some(1), &[]);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Builder t1 r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::FailedUnverified,
        error: Some(
            "failed_unverified: missing finish stamp at artifacts/run-finish/coder-t1-r1-a1.toml"
                .to_string(),
        ),
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });

    let nodes = build_tree(&state);
    fn find_run_node(node: &Node) -> Option<&Node> {
        if node.run_id == Some(1) || node.leaf_run_id == Some(1) {
            return Some(node);
        }
        node.children.iter().find_map(find_run_node)
    }
    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let attempt = find_run_node(builder).expect("run node");

    assert_eq!(attempt.status, NodeStatus::FailedUnverified);
}

#[test]
fn test_collapsed_stage_leaf_run_id() {
    let mut state = SessionState::new("test".to_string());
    state.agent_runs.push(run(1, "brainstorm", RunStatus::Done));
    let nodes = build_tree(&state);
    let brainstorm = nodes.iter().find(|n| n.label == "Brainstorm").unwrap();
    assert_eq!(brainstorm.leaf_run_id, Some(1));
    assert!(brainstorm.children.is_empty());
}

#[test]
fn builder_stage_orders_done_recovery_current_pending() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::BuilderRecovery(4);
    set_builder_tasks(&mut state, &[3, 1], Some(9), &[8, 7]);
    state.agent_runs.push(RunRecord {
        id: 99,
        stage: "recovery".to_string(),
        task_id: None,
        round: 4,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "codex".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    let nodes = build_tree(&state);
    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let labels = builder
        .children
        .iter()
        .map(|child| child.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec!["Task 3", "Task 1", "Task 9", "Task 8", "Task 7"]
    );
    let task9 = builder
        .children
        .iter()
        .find(|child| child.label == "Task 9")
        .expect("current task");
    assert!(
        task9
            .children
            .iter()
            .any(|child| child.label == "Builder Recovery"),
        "Builder Recovery should be nested inside the blocked/current task"
    );
    assert_eq!(builder.summary, "builder recovery in progress");
    assert_eq!(builder.status, NodeStatus::Running);
}

#[test]
fn builder_recovery_uses_trigger_task_for_position() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::BuilderRecovery(4);
    set_builder_tasks(&mut state, &[1], Some(2), &[3]);
    state.builder.iteration = 4;
    state.builder.recovery_trigger_task_id = Some(2);
    let mut recovery = run(99, "recovery", RunStatus::Running);
    recovery.round = 4;
    state.agent_runs.push(recovery);

    let nodes = build_tree(&state);
    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let labels = builder
        .children
        .iter()
        .map(|child| child.label.as_str())
        .collect::<Vec<_>>();

    assert_eq!(labels, vec!["Task 1", "Task 2", "Task 3"]);
    let task2 = builder
        .children
        .iter()
        .find(|child| child.label == "Task 2")
        .expect("trigger task");
    let task2_labels = task2
        .children
        .iter()
        .map(|child| child.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        task2_labels,
        vec!["Round 4", "Builder Recovery"],
        "Builder Recovery should be inside the trigger task after the blocked round"
    );
}

#[test]
fn builder_recovery_sits_between_blocked_round_and_new_round_inside_task() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(5);
    set_builder_tasks(&mut state, &[], Some(2), &[]);
    state.builder.recovery_trigger_task_id = Some(2);

    let mut coder_round4 = run(10, "coder", RunStatus::Done);
    coder_round4.task_id = Some(2);
    coder_round4.round = 4;
    state.agent_runs.push(coder_round4);
    let mut reviewer_round4 = run(11, "reviewer", RunStatus::Failed);
    reviewer_round4.task_id = Some(2);
    reviewer_round4.round = 4;
    state.agent_runs.push(reviewer_round4);
    let mut recovery = run(12, "recovery", RunStatus::Done);
    recovery.round = 4;
    state.agent_runs.push(recovery);
    let mut coder_round5 = run(13, "coder", RunStatus::Running);
    coder_round5.task_id = Some(2);
    coder_round5.round = 5;
    state.agent_runs.push(coder_round5);

    let nodes = build_tree(&state);
    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let task2 = builder
        .children
        .iter()
        .find(|child| child.label == "Task 2")
        .expect("task 2");
    let task2_labels = task2
        .children
        .iter()
        .map(|child| child.label.as_str())
        .collect::<Vec<_>>();

    assert_eq!(task2_labels, vec!["Round 4", "Builder Recovery", "Round 5"]);
}

#[test]
fn recovery_rounds_include_sharding_run_sharing_recovery_round_without_pipeline_mode() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::BuilderRecoverySharding(6);
    set_builder_tasks(&mut state, &[1, 2], None, &[3]);

    let mut recovery = run(3, "recovery", RunStatus::Done);
    recovery.round = 6;
    state.agent_runs.push(recovery);
    let mut sharding = run(5, "sharding", RunStatus::Running);
    sharding.round = 6;
    state.agent_runs.push(sharding);

    let nodes = build_tree(&state);
    let sharding_stage = nodes.iter().find(|n| n.label == "Sharding").unwrap();
    let mut top_level_sharding_ids = Vec::new();
    collect_run_ids(sharding_stage, &mut top_level_sharding_ids);
    assert!(
        !top_level_sharding_ids.contains(&5),
        "recovery sharding run leaked into top-level Sharding: {top_level_sharding_ids:?}"
    );

    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let recovery = builder
        .children
        .iter()
        .find(|child| child.label == "Builder Recovery")
        .expect("Builder Recovery node missing");
    let labels = recovery
        .children
        .iter()
        .map(|child| child.label.as_str())
        .collect::<Vec<_>>();

    assert!(
        labels.contains(&"Recovery") && labels.contains(&"Sharding"),
        "Builder Recovery should show both same-round recovery modes, got: {labels:?}"
    );
    let mut recovery_ids = Vec::new();
    collect_run_ids(recovery, &mut recovery_ids);
    assert!(
        recovery_ids.contains(&5),
        "Builder Recovery missing same-round sharding run: {recovery_ids:?}"
    );
}

#[test]
fn recovery_plan_review_and_sharding_route_under_builder_recovery() {
    use crate::state::PipelineItem;
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::BuilderRecoverySharding(6);
    set_builder_tasks(&mut state, &[1, 2], None, &[3]);
    state.builder.iteration = 6;
    // Original (round 1) plan-review and sharding runs.
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "plan-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gemini".to_string(),
        subscription_label: "google".to_string(),
        window_name: "[Plan Review 1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "sharding".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Sharding]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    // Recovery plan-review and sharding runs (round 6) — should be routed
    // under Builder Recovery, not the top-level stages.
    state.agent_runs.push(RunRecord {
        id: 3,
        stage: "recovery".to_string(),
        task_id: None,
        round: 6,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "codex".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 4,
        stage: "plan-review".to_string(),
        task_id: None,
        round: 6,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "codex".to_string(),
        window_name: "[Recovery Plan Review]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 5,
        stage: "sharding".to_string(),
        task_id: None,
        round: 6,
        attempt: 1,
        model: "claude".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Recovery Sharding]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    // Pipeline items mark the recovery rounds.
    state.builder.pipeline_items.push(PipelineItem {
        id: 10,
        stage: "plan-review".to_string(),
        task_id: None,
        round: Some(6),
        status: PipelineItemStatus::Done,
        title: Some("Recovery plan review".to_string()),
        mode: Some("recovery".to_string()),
        trigger: None,
        interactive: Some(false),
        iteration: 1,
    });
    state.builder.pipeline_items.push(PipelineItem {
        id: 11,
        stage: "sharding".to_string(),
        task_id: None,
        round: Some(6),
        status: PipelineItemStatus::Running,
        title: Some("Recovery sharding".to_string()),
        mode: Some("recovery".to_string()),
        trigger: None,
        interactive: Some(false),
        iteration: 1,
    });

    let nodes = build_tree(&state);

    // Top-level Plan Review and Sharding stages must NOT contain round-6 runs.
    let plan_review = nodes.iter().find(|n| n.label == "Plan Review").unwrap();
    let sharding = nodes.iter().find(|n| n.label == "Sharding").unwrap();
    assert_eq!(plan_review.status, NodeStatus::Done);
    assert_eq!(sharding.status, NodeStatus::Done);

    let mut pr_ids = Vec::new();
    collect_run_ids(plan_review, &mut pr_ids);
    assert!(
        !pr_ids.contains(&4),
        "recovery plan-review run leaked into top-level Plan Review: {pr_ids:?}"
    );
    let mut sh_ids = Vec::new();
    collect_run_ids(sharding, &mut sh_ids);
    assert!(
        !sh_ids.contains(&5),
        "recovery sharding run leaked into top-level Sharding: {sh_ids:?}"
    );

    // Builder Recovery sub-tree must contain all three recovery runs.
    let builder = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let recovery = builder
        .children
        .iter()
        .find(|c| c.label == "Builder Recovery")
        .expect("Builder Recovery node missing");
    let mut rec_ids = Vec::new();
    collect_run_ids(recovery, &mut rec_ids);
    for expected in [3u64, 4, 5] {
        assert!(
            rec_ids.contains(&expected),
            "Builder Recovery missing run {expected}: {rec_ids:?}"
        );
    }
    assert_eq!(recovery.status, NodeStatus::Running);
}

#[test]
fn final_validation_gap_tasks_render_under_new_iteration_trio() {
    // Regression for the chronology bug: tasks added by an FV goal_gap
    // verdict carry iteration 2 and must land under their own
    // (Loop, Simplification, FinalValidation) trio so their later-round
    // messages render after FV[1] in the dashboard's top-down order,
    // not before it.
    use crate::state::PipelineItem;
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(2);
    state.builder.task_titles.insert(1, "original".to_string());
    state
        .builder
        .task_titles
        .insert(2, "validator gap".to_string());
    state.builder.pipeline_items.push(PipelineItem {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Approved,
        title: Some("original".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 1,
    });
    state.builder.pipeline_items.push(PipelineItem {
        id: 2,
        stage: "coder".to_string(),
        task_id: Some(2),
        round: Some(2),
        status: PipelineItemStatus::Pending,
        title: Some("validator gap".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 2,
    });
    let mut coder1 = run(11, "coder", RunStatus::Done);
    coder1.task_id = Some(1);
    coder1.round = 1;
    coder1.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(coder1);
    let mut fv1 = run(20, "final-validation", RunStatus::Done);
    fv1.round = 1;
    fv1.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(fv1);
    let mut coder2 = run(31, "coder", RunStatus::Running);
    coder2.task_id = Some(2);
    coder2.round = 2;
    state.agent_runs.push(coder2);

    let nodes = build_tree(&state);
    let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();

    // Iteration 1 trio must precede iteration 2 trio (chronological order).
    let pos = |label: &str| {
        labels
            .iter()
            .position(|l| *l == label)
            .unwrap_or_else(|| panic!("missing top-level node {label} (got {labels:?})"))
    };
    assert!(pos("Loop") < pos("Final Validation"));
    assert!(pos("Final Validation") < pos("Loop · iteration 2"));
    assert!(pos("Loop · iteration 2") < pos("Final Validation · iteration 2"));

    // Original task lives under Loop[1]; validator-gap task only under Loop[2].
    let loop1 = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let loop2 = nodes
        .iter()
        .find(|n| n.label == "Loop · iteration 2")
        .unwrap();
    let task_labels = |stage: &Node| -> Vec<String> {
        stage
            .children
            .iter()
            .filter(|c| c.kind == NodeKind::Task)
            .map(|c| c.label.clone())
            .collect()
    };
    let loop1_tasks = task_labels(loop1);
    let loop2_tasks = task_labels(loop2);
    assert!(
        loop1_tasks.iter().any(|t| t.starts_with("Task 1")),
        "Loop[1] must contain Task 1: {loop1_tasks:?}"
    );
    assert!(
        loop1_tasks.iter().all(|t| !t.starts_with("Task 2")),
        "Loop[1] must NOT contain validator-gap Task 2: {loop1_tasks:?}"
    );
    assert!(
        loop2_tasks.iter().any(|t| t.starts_with("Task 2")),
        "Loop[2] must contain Task 2: {loop2_tasks:?}"
    );

    // Run-id chronology: round-1 runs only in iteration-1 trio,
    // round-2 runs only in iteration-2 trio.
    let mut iter1_runs = Vec::new();
    collect_run_ids(loop1, &mut iter1_runs);
    let mut iter2_runs = Vec::new();
    collect_run_ids(loop2, &mut iter2_runs);
    assert!(iter1_runs.contains(&11), "round-1 coder under Loop[1]");
    assert!(
        !iter1_runs.contains(&31),
        "round-2 coder must not leak into Loop[1]: {iter1_runs:?}"
    );
    assert!(iter2_runs.contains(&31), "round-2 coder under Loop[2]");
    assert!(
        !iter2_runs.contains(&11),
        "round-1 coder must not leak into Loop[2]: {iter2_runs:?}"
    );
}

#[test]
fn test_collapse_preserves_mode_absorbs_attempt() {
    // Mode nodes are never absorbed but can absorb single AgentRun children
    let mut task = Node {
        label: "Task 1".to_string(),
        kind: NodeKind::Task,
        status: NodeStatus::Done,
        summary: "".to_string(),
        children: vec![Node {
            label: "Round 1".to_string(),
            kind: NodeKind::Round,
            status: NodeStatus::Done,
            summary: "".to_string(),
            children: vec![
                Node {
                    label: "Builder".to_string(),
                    kind: NodeKind::Mode,
                    status: NodeStatus::Done,
                    summary: "".to_string(),
                    children: vec![Node {
                        label: "Attempt 1".to_string(),
                        kind: NodeKind::AgentRun,
                        status: NodeStatus::Done,
                        summary: "".to_string(),
                        children: vec![],
                        run_id: Some(1),
                        leaf_run_id: None,
                    }],
                    run_id: None,
                    leaf_run_id: None,
                },
                Node {
                    label: "Reviewer".to_string(),
                    kind: NodeKind::Mode,
                    status: NodeStatus::Done,
                    summary: "".to_string(),
                    children: vec![Node {
                        label: "Attempt 1".to_string(),
                        kind: NodeKind::AgentRun,
                        status: NodeStatus::Done,
                        summary: "".to_string(),
                        children: vec![],
                        run_id: Some(2),
                        leaf_run_id: None,
                    }],
                    run_id: None,
                    leaf_run_id: None,
                },
            ],
            run_id: None,
            leaf_run_id: None,
        }],
        run_id: None,
        leaf_run_id: None,
    };
    collapse_tree(&mut task);
    // Task absorbs Round, but Round had 2 Mode children (multi-child preserved)
    // So Task now has the 2 Mode children; each Mode absorbed its single AgentRun
    assert_eq!(task.children.len(), 2);
    assert_eq!(task.children[0].label, "Builder");
    assert_eq!(task.children[0].kind, NodeKind::Mode);
    assert_eq!(task.children[0].leaf_run_id, Some(1));
    assert_eq!(task.children[1].label, "Reviewer");
    assert_eq!(task.children[1].kind, NodeKind::Mode);
    assert_eq!(task.children[1].leaf_run_id, Some(2));
}

#[test]
fn test_collapse_mode_multiple_attempts_preserved() {
    // Mode with multiple attempts preserves them as children
    let mut mode = Node {
        label: "Builder".to_string(),
        kind: NodeKind::Mode,
        status: NodeStatus::Done,
        summary: "".to_string(),
        children: vec![
            Node {
                label: "Attempt 1".to_string(),
                kind: NodeKind::AgentRun,
                status: NodeStatus::Failed,
                summary: "".to_string(),
                children: vec![],
                run_id: Some(1),
                leaf_run_id: None,
            },
            Node {
                label: "Attempt 2".to_string(),
                kind: NodeKind::AgentRun,
                status: NodeStatus::Done,
                summary: "".to_string(),
                children: vec![],
                run_id: Some(2),
                leaf_run_id: None,
            },
        ],
        run_id: None,
        leaf_run_id: None,
    };
    collapse_tree(&mut mode);
    // Mode keeps both attempt children (multi-child not collapsed)
    assert_eq!(mode.children.len(), 2);
    assert_eq!(mode.leaf_run_id, None);
}

#[test]
fn node_keys_distinguish_duplicate_mode_labels_by_ancestry() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(2);
    set_builder_tasks(&mut state, &[7], Some(8), &[]);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 2,
        attempt: 1,
        model: "claude".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Builder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "coder".to_string(),
        task_id: Some(8),
        round: 2,
        attempt: 1,
        model: "gpt".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Builder 2]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });

    let nodes = build_tree(&state);
    let rows = collect_all_rows(&nodes);
    let coder_rows = rows
        .into_iter()
        .filter(|row| node_at_path(&nodes, &row.path).is_some_and(|node| node.label == "Builder"))
        .collect::<Vec<_>>();

    assert_eq!(coder_rows.len(), 2);
    assert_ne!(coder_rows[0].key, coder_rows[1].key);
}

#[test]
fn flatten_visible_rows_hides_collapsed_descendants() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ReviewRound(1);
    set_builder_tasks(&mut state, &[], Some(3), &[]);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(3),
        round: 1,
        attempt: 1,
        model: "claude".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: "[Builder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });

    let nodes = build_tree(&state);
    let all_rows = collect_all_rows(&nodes);
    let task_row = all_rows
        .iter()
        .find(|row| node_at_path(&nodes, &row.path).is_some_and(|node| node.label == "Task 3"))
        .cloned()
        .expect("task row");
    let stage_row = all_rows
        .iter()
        .find(|row| row.path == vec![6])
        .cloned()
        .expect("builder stage row");

    let visible = flatten_visible_rows(&nodes, |row| row.key == stage_row.key);

    assert!(visible.iter().any(|row| row.key == task_row.key));
    assert!(
        visible
            .iter()
            .all(|row| node_at_path(&nodes, &row.path).is_none_or(|node| node.label != "Builder"))
    );
}

#[test]
fn active_stage_paths_prefer_latest_running_leaf() {
    let earlier = chrono::Utc::now() - chrono::Duration::minutes(5);
    let later = chrono::Utc::now();
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::SpecReviewRunning;
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Spec Review 1]".to_string(),
        started_at: earlier,
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 2,
        attempt: 1,
        model: "o3".to_string(),
        subscription_label: "openai".to_string(),
        window_name: "[Spec Review 2]".to_string(),
        started_at: later,
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    });

    let nodes = build_tree(&state);
    let stage_key = node_key_at_path(&nodes, &[2]).expect("spec review key");
    let active = active_stage_paths(&nodes, &state.agent_runs);
    let chosen = active.get(&stage_key).expect("active descendant");
    let chosen_node = node_at_path(&nodes, chosen).expect("chosen node");

    assert_eq!(chosen_node.run_id.or(chosen_node.leaf_run_id), Some(2));
}

#[test]
fn test_agent_run_node_tough_suffix() {
    // Suffix now derives from the per-tuple `effort_mapping` +
    // `effort_eligible` mirrored onto each RunRecord at launch time, not
    // from a vendor-string lookup. Codex/Claude rows ship effort-capable
    // mappings (Codex tough=`xhigh`, Claude tough=`max`); non-effort rows
    // (Gemini-style) carry the default mapping but `effort_eligible=false`,
    // suppressing the suffix even at Tough.
    use crate::data::config::schema::EffortMapping;

    let mut normal_run = run(1, "coder", RunStatus::Running);
    normal_run.effort = crate::adapters::EffortLevel::Normal;
    normal_run.effort_eligible = true;
    normal_run.effort_mapping = EffortMapping::new("low", "medium", "xhigh");
    let node = agent_run_node(&normal_run);
    assert!(
        !node.label.contains(":xhigh") && !node.label.contains(":max"),
        "Normal run should not have effort suffix, got: {}",
        node.label
    );

    let mut tough_codex = run(2, "coder", RunStatus::Running);
    tough_codex.effort = crate::adapters::EffortLevel::Tough;
    tough_codex.effort_eligible = true;
    tough_codex.effort_mapping = EffortMapping::new("low", "medium", "xhigh");
    let node = agent_run_node(&tough_codex);
    assert!(
        node.label.ends_with(":xhigh"),
        "Tough codex run should end with :xhigh, got: {}",
        node.label
    );

    let mut tough_claude = run(3, "coder", RunStatus::Running);
    tough_claude.effort = crate::adapters::EffortLevel::Tough;
    tough_claude.effort_eligible = true;
    tough_claude.effort_mapping = EffortMapping::new("low", "medium", "max");
    let node = agent_run_node(&tough_claude);
    assert!(
        node.label.ends_with(":max"),
        "Tough claude run should end with :max, got: {}",
        node.label
    );

    let mut tough_gemini = run(4, "coder", RunStatus::Running);
    tough_gemini.effort = crate::adapters::EffortLevel::Tough;
    tough_gemini.effort_eligible = false;
    tough_gemini.effort_mapping = EffortMapping::default();
    let node = agent_run_node(&tough_gemini);
    assert!(
        !node.label.contains(":xhigh") && !node.label.contains(":max"),
        "Tough gemini run should have no effort suffix, got: {}",
        node.label
    );

    let mut low_codex = run(5, "coder", RunStatus::Running);
    low_codex.effort = crate::adapters::EffortLevel::Low;
    low_codex.effort_eligible = true;
    low_codex.effort_mapping = EffortMapping::new("low", "medium", "xhigh");
    let node = agent_run_node(&low_codex);
    assert!(
        node.label.ends_with(":low"),
        "Low codex run should end with :low, got: {}",
        node.label
    );
}

#[test]
fn test_task_node_tough_badge() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    set_builder_tasks(&mut state, &[], Some(1), &[2]);

    let normal_run = RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "gpt-5.2".to_string(),
        subscription_label: "codex".to_string(),
        window_name: "[Round 1 Coder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    };
    state.agent_runs.push(normal_run.clone());

    let mut tough_run = normal_run.clone();
    tough_run.id = 2;
    tough_run.task_id = Some(2);
    tough_run.effort = crate::adapters::EffortLevel::Tough;
    state.agent_runs.push(tough_run);

    let nodes = build_tree(&state);
    let loop_node = nodes.iter().find(|n| n.label == "Loop").unwrap();
    let task1 = loop_node
        .children
        .iter()
        .find(|n| n.label.starts_with("Task 1"))
        .unwrap();
    assert!(
        !task1.label.contains("[tough]"),
        "Normal task should not have [tough], got: {}",
        task1.label
    );
    let task2 = loop_node
        .children
        .iter()
        .find(|n| n.label.starts_with("Task 2"))
        .unwrap();
    assert!(
        task2.label.ends_with("[tough]"),
        "Tough task should end with [tough], got: {}",
        task2.label
    );
}

#[test]
fn final_validation_running_renders_as_normal_stage() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::FinalValidation(2);
    let mut validator = run(42, "final-validation", RunStatus::Running);
    validator.round = 2;
    validator.window_name = "[FinalValidation] opus".to_string();
    state.agent_runs.push(validator);

    let nodes = build_tree(&state);
    let stage = nodes
        .iter()
        .find(|n| n.label == "Final Validation")
        .expect("final validation stage missing");
    assert_eq!(stage.kind, NodeKind::Stage);
    assert_eq!(stage.status, NodeStatus::Running);
    assert_eq!(stage.summary, "final validation running");
    // Single round + single attempt collapses, so the stage carries the leaf
    // run id directly — that is what wires the live-summary tail to the
    // stage row in the dashboard.
    assert_eq!(stage.leaf_run_id, Some(42));
}

#[test]
fn final_validation_pending_before_validation_phase() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::PlanningRunning;
    let nodes = build_tree(&state);
    let stage = nodes
        .iter()
        .find(|n| n.label == "Final Validation")
        .unwrap();
    assert_eq!(stage.status, NodeStatus::Pending);
    assert!(stage.children.is_empty());
}

#[test]
fn final_validation_skipped_under_yolo_done() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Done;
    state.modes.yolo = true;
    let nodes = build_tree(&state);
    let stage = nodes
        .iter()
        .find(|n| n.label == "Final Validation")
        .unwrap();
    assert_eq!(stage.status, NodeStatus::Skipped);
}

#[test]
fn final_validation_runs_split_across_iteration_trios() {
    // Each FV run closes its own outer iteration, so the dashboard now
    // renders two `Final Validation` stages — one per iteration — instead
    // of aggregating both rounds under a single node.
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::FinalValidation(2);
    let mut r1 = run(10, "final-validation", RunStatus::Done);
    r1.round = 1;
    r1.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(r1);
    let mut r2 = run(20, "final-validation", RunStatus::Running);
    r2.round = 2;
    state.agent_runs.push(r2);

    let nodes = build_tree(&state);

    let fv1 = nodes
        .iter()
        .find(|n| n.label == "Final Validation")
        .expect("iteration 1 FV present");
    let mut fv1_runs = Vec::new();
    collect_run_ids(fv1, &mut fv1_runs);
    assert!(fv1_runs.contains(&10), "FV[1] holds round-1 run 10");
    assert!(
        !fv1_runs.contains(&20),
        "round-2 FV must not appear inside FV[1]: {fv1_runs:?}"
    );

    let fv2 = nodes
        .iter()
        .find(|n| n.label == "Final Validation · iteration 2")
        .expect("iteration 2 FV present");
    let mut fv2_runs = Vec::new();
    collect_run_ids(fv2, &mut fv2_runs);
    assert!(fv2_runs.contains(&20), "FV[2] holds round-2 run 20");
    assert_eq!(fv2.status, NodeStatus::Running);
}

#[test]
fn simplification_running_renders_as_normal_stage() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Simplification(2);
    let mut simplifier = run(77, "simplifier", RunStatus::Running);
    simplifier.round = 2;
    simplifier.window_name = "[Simplifier] opus".to_string();
    state.agent_runs.push(simplifier);

    let nodes = build_tree(&state);
    let stage = nodes
        .iter()
        .find(|n| n.label == "Simplification")
        .expect("simplification stage missing");
    assert_eq!(stage.kind, NodeKind::Stage);
    assert_eq!(stage.status, NodeStatus::Running);
    assert_eq!(stage.summary, "simplification running");
    assert_eq!(stage.leaf_run_id, Some(77));
}

#[test]
fn simplification_pending_before_simplification_phase() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    let nodes = build_tree(&state);
    let stage = nodes.iter().find(|n| n.label == "Simplification").unwrap();
    assert_eq!(stage.status, NodeStatus::Pending);
    assert!(stage.children.is_empty());
}

#[test]
fn simplification_precedes_final_validation_in_tree_order() {
    let state = SessionState::new("test".to_string());
    let nodes = build_tree(&state);
    let simpl_index = nodes
        .iter()
        .position(|n| n.label == "Simplification")
        .expect("simplification stage missing");
    let final_index = nodes
        .iter()
        .position(|n| n.label == "Final Validation")
        .expect("final validation stage missing");
    assert!(
        simpl_index < final_index,
        "Simplification must appear before Final Validation in the tree, got simpl={} final={}",
        simpl_index,
        final_index
    );
}

#[test]
fn dreaming_renders_after_final_validation_as_global_stage() {
    let state = SessionState::new("test".to_string());
    let nodes = build_tree(&state);
    let final_index = nodes
        .iter()
        .position(|n| n.label == "Final Validation")
        .expect("final validation stage missing");
    let dreaming_index = nodes
        .iter()
        .position(|n| n.label == "Dreaming")
        .expect("dreaming stage missing");
    assert!(
        final_index < dreaming_index,
        "Dreaming must render after the global final validation stage"
    );
}

#[test]
fn dreaming_pending_shows_waiting_user() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::DreamingPending;
    state.dreaming_decision = Some(crate::state::DreamingDecision {
        kind: crate::state::DreamingDecisionKind::Pending,
        round: 1,
        reason: Some("Consolidate lessons.".to_string()),
    });
    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    assert_eq!(dreaming.status, NodeStatus::WaitingUser);
}

#[test]
fn dreaming_running_shows_running() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Dreaming(1);
    let mut run = run(50, "dreaming", RunStatus::Running);
    run.round = 1;
    state.agent_runs.push(run);
    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    assert_eq!(dreaming.status, NodeStatus::Running);
}

#[test]
fn dreaming_failed_shows_failed() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Dreaming(1);
    state.agent_error = Some("invalid report".to_string());
    let mut run = run(50, "dreaming", RunStatus::Failed);
    run.round = 1;
    state.agent_runs.push(run);
    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    assert_eq!(dreaming.status, NodeStatus::Failed);
}

#[test]
fn dreaming_skipped_shows_skipped() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Done;
    state.dreaming_decision = Some(crate::state::DreamingDecision {
        kind: crate::state::DreamingDecisionKind::OperatorSkipped,
        round: 1,
        reason: None,
    });
    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    assert_eq!(dreaming.status, NodeStatus::Skipped);
}

#[test]
fn dreaming_done_shows_done() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Done;
    state.dreaming_decision = Some(crate::state::DreamingDecision {
        kind: crate::state::DreamingDecisionKind::OperatorAccepted,
        round: 1,
        reason: None,
    });
    let mut run = run(50, "dreaming", RunStatus::Done);
    run.round = 1;
    state.agent_runs.push(run);
    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    assert_eq!(dreaming.status, NodeStatus::Done);
}

#[test]
fn dreaming_groups_runs_by_round() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Dreaming(2);
    let mut r1 = run(50, "dreaming", RunStatus::Done);
    r1.round = 1;
    r1.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(r1);
    let mut r2 = run(60, "dreaming", RunStatus::Running);
    r2.round = 2;
    state.agent_runs.push(r2);

    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    let mut run_ids = Vec::new();
    collect_run_ids(dreaming, &mut run_ids);
    assert!(run_ids.contains(&50));
    assert!(run_ids.contains(&60));
    assert_eq!(dreaming.status, NodeStatus::Running);
}

#[test]
fn dreaming_not_reoffered_after_completion() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Done;
    state.dreaming_decision = Some(crate::state::DreamingDecision {
        kind: crate::state::DreamingDecisionKind::OperatorAccepted,
        round: 1,
        reason: None,
    });
    let mut run = run(50, "dreaming", RunStatus::Done);
    run.round = 1;
    state.agent_runs.push(run);
    let nodes = build_tree(&state);
    let dreaming = nodes.iter().find(|n| n.label == "Dreaming").unwrap();
    assert_eq!(dreaming.status, NodeStatus::Done);
}

#[test]
fn simplification_skipped_under_yolo_done() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Done;
    state.modes.yolo = true;
    let nodes = build_tree(&state);
    let stage = nodes.iter().find(|n| n.label == "Simplification").unwrap();
    assert_eq!(stage.status, NodeStatus::Skipped);
}

#[test]
fn simplification_groups_runs_by_round() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Simplification(2);
    let mut r1 = run(30, "simplifier", RunStatus::Done);
    r1.round = 1;
    r1.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(r1);
    let mut r2 = run(40, "simplifier", RunStatus::Running);
    r2.round = 2;
    state.agent_runs.push(r2);

    let nodes = build_tree(&state);
    let stage = nodes.iter().find(|n| n.label == "Simplification").unwrap();
    let mut run_ids = Vec::new();
    collect_run_ids(stage, &mut run_ids);
    assert!(run_ids.contains(&30));
    assert!(run_ids.contains(&40));
    assert_eq!(stage.status, NodeStatus::Running);
}

#[test]
fn builder_loop_done_during_simplification_phase() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Simplification(1);
    set_builder_tasks(&mut state, &[1], None, &[]);
    let mut coder = run(1, "coder", RunStatus::Done);
    coder.task_id = Some(1);
    coder.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(coder);
    let mut reviewer = run(2, "reviewer", RunStatus::Done);
    reviewer.task_id = Some(1);
    reviewer.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(reviewer);

    let nodes = build_tree(&state);
    let loop_node = nodes
        .iter()
        .find(|n| n.label == "Loop")
        .expect("Loop stage missing");
    assert_eq!(
        loop_node.status,
        NodeStatus::Done,
        "Loop should be Done while simplification runs (was {:?})",
        loop_node.status,
    );
}

#[test]
fn builder_loop_done_during_final_validation_phase() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::FinalValidation(1);
    set_builder_tasks(&mut state, &[1], None, &[]);
    let mut coder = run(1, "coder", RunStatus::Done);
    coder.task_id = Some(1);
    coder.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(coder);
    let mut reviewer = run(2, "reviewer", RunStatus::Done);
    reviewer.task_id = Some(1);
    reviewer.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(reviewer);

    let nodes = build_tree(&state);
    let loop_node = nodes
        .iter()
        .find(|n| n.label == "Loop")
        .expect("Loop stage missing");
    assert_eq!(
        loop_node.status,
        NodeStatus::Done,
        "Loop should be Done while final validation runs (was {:?})",
        loop_node.status,
    );
}

/// Walk a single subtree and count Round nodes whose ancestor chain
/// contains a node labelled `task_label` and whose own label equals
/// `round_label`. Inline helper — defined here rather than as a shared
/// util to keep the diff local.
#[cfg(test)]
fn count_round_nodes_under_task(node: &Node, task_label: &str, round_label: &str) -> usize {
    fn walk(node: &Node, in_task: bool, task_label: &str, round_label: &str, count: &mut usize) {
        let now_in_task = in_task || node.label == task_label;
        if now_in_task && node.kind == NodeKind::Round && node.label == round_label {
            *count += 1;
        }
        for child in &node.children {
            walk(child, now_in_task, task_label, round_label, count);
        }
    }
    let mut count = 0;
    walk(node, false, task_label, round_label, &mut count);
    count
}

#[test]
fn non_contiguous_same_round_runs_emit_sibling_round_nodes() {
    use super::stages::build_builder_stage;
    use crate::state::PipelineItem;
    let mut state = SessionState::new("non-contig".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.task_titles.insert(1, "alpha".to_string());
    state.builder.task_titles.insert(2, "beta".to_string());
    state.builder.pipeline_items.push(PipelineItem {
        id: 100,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Done,
        title: Some("alpha".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 1,
    });
    state.builder.pipeline_items.push(PipelineItem {
        id: 101,
        stage: "coder".to_string(),
        task_id: Some(2),
        round: Some(1),
        status: PipelineItemStatus::Done,
        title: Some("beta".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 1,
    });
    // Run 1: task 1, round 1 — earliest.
    let mut r1 = run(1, "coder", RunStatus::Done);
    r1.task_id = Some(1);
    r1.round = 1;
    state.agent_runs.push(r1);
    // Run 2: task 2, round 1 — same round, different task, interleaved.
    let mut r2 = run(2, "coder", RunStatus::Done);
    r2.task_id = Some(2);
    r2.round = 1;
    state.agent_runs.push(r2);
    // Run 3: task 1, round 1 again — chronologically AFTER task 2's run.
    let mut r3 = run(3, "coder", RunStatus::Done);
    r3.task_id = Some(1);
    r3.round = 1;
    state.agent_runs.push(r3);

    // Inspect the pre-collapse builder stage so collapse_tree's single-child
    // Round absorption doesn't mask the contiguity invariant.
    let loop_node = build_builder_stage(&state, 1);
    let task1_round1 = count_round_nodes_under_task(&loop_node, "Task 1: alpha", "Round 1");
    assert_eq!(
        task1_round1, 2,
        "task 1 round 1 must appear twice — separated by task 2's round-1 run.\nTree:\n{loop_node:#?}"
    );
}

#[test]
fn contiguous_same_round_attempts_nest_under_one_round_node() {
    use super::stages::build_builder_stage;
    use crate::state::PipelineItem;
    let mut state = SessionState::new("contig".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.task_titles.insert(1, "alpha".to_string());
    state.builder.pipeline_items.push(PipelineItem {
        id: 100,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(1),
        status: PipelineItemStatus::Done,
        title: Some("alpha".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 1,
    });
    // Two adjacent coder attempts at task 1, round 1.
    let mut r1 = run(1, "coder", RunStatus::Failed);
    r1.task_id = Some(1);
    r1.round = 1;
    r1.attempt = 1;
    state.agent_runs.push(r1);
    let mut r2 = run(2, "coder", RunStatus::Done);
    r2.task_id = Some(1);
    r2.round = 1;
    r2.attempt = 2;
    state.agent_runs.push(r2);

    let loop_node = build_builder_stage(&state, 1);
    let task1_round1 = count_round_nodes_under_task(&loop_node, "Task 1: alpha", "Round 1");
    assert_eq!(
        task1_round1, 1,
        "two attempts at the same round must nest under ONE Round node.\nTree:\n{loop_node:#?}"
    );
}
