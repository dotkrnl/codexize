use super::*;

fn run(id: u64, stage: &str, status: RunStatus) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Test]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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

#[test]
fn test_build_tree_single_stage() {
    let mut state = SessionState::new("test".to_string());
    state.agent_runs.push(run(1, "brainstorm", RunStatus::Done));
    let nodes = build_tree(&state);
    assert_eq!(nodes.len(), 9); // Idea + 6 stages + Simplification + Final Validation
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
        vendor: "openai".to_string(),
        window_name: "[Spec Review r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Failed,
        error: Some("quota exceeded".to_string()),
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 2,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Spec Review r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
        vendor: "openai".to_string(),
        window_name: "[Planning]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Failed,
        error: Some("quota exceeded".to_string()),
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt: 2,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Planning]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
    state.builder.current_task = Some(1);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Builder t1 r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Failed,
        error: Some("timeout".to_string()),
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 2,
        model: "claude-opus-4-7".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Builder t1 r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
    state.builder.current_task = Some(1);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder t1 r1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::FailedUnverified,
        error: Some(
            "failed_unverified: missing finish stamp at artifacts/run-finish/coder-t1-r1-a1.toml"
                .to_string(),
        ),
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
    state.builder.done = vec![3, 1];
    state.builder.current_task = Some(9);
    state.builder.pending = vec![8, 7];
    state.agent_runs.push(RunRecord {
        id: 99,
        stage: "recovery".to_string(),
        task_id: None,
        round: 4,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "codex".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
        vec![
            "Task 3",
            "Task 1",
            "Builder Recovery",
            "Task 9",
            "Task 8",
            "Task 7",
        ]
    );
    assert_eq!(builder.summary, "builder recovery in progress");
    assert_eq!(builder.status, NodeStatus::Running);
}

#[test]
fn builder_recovery_uses_trigger_task_for_position() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::BuilderRecovery(4);
    state.builder.done = vec![1];
    state.builder.current_task = Some(2);
    state.builder.pending = vec![3];
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

    assert_eq!(
        labels,
        vec!["Task 1", "Task 2", "Builder Recovery", "Task 3"]
    );
}

#[test]
fn recovery_rounds_include_sharding_run_sharing_recovery_round_without_pipeline_mode() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::BuilderRecoverySharding(6);
    state.builder.done = vec![1, 2];
    state.builder.pending = vec![3];

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
    state.builder.done = vec![1, 2];
    state.builder.pending = vec![3];
    state.builder.iteration = 6;
    // Original (round 1) plan-review and sharding runs.
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "plan-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gemini".to_string(),
        vendor: "google".to_string(),
        window_name: "[Plan Review 1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "sharding".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Sharding]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
        vendor: "codex".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 4,
        stage: "plan-review".to_string(),
        task_id: None,
        round: 6,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "codex".to_string(),
        window_name: "[Recovery Plan Review]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 5,
        stage: "sharding".to_string(),
        task_id: None,
        round: 6,
        attempt: 1,
        model: "claude".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Recovery Sharding]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
    state.builder.done = vec![7];
    state.builder.current_task = Some(8);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 2,
        attempt: 1,
        model: "claude".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Builder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "coder".to_string(),
        task_id: Some(8),
        round: 2,
        attempt: 1,
        model: "gpt".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder 2]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
    state.builder.current_task = Some(3);
    state.agent_runs.push(RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(3),
        round: 1,
        attempt: 1,
        model: "claude".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Builder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
        vendor: "openai".to_string(),
        window_name: "[Spec Review 1]".to_string(),
        started_at: earlier,
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 2,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 2,
        attempt: 1,
        model: "o3".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Spec Review 2]".to_string(),
        started_at: later,
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
    let mut normal_run = run(1, "coder", RunStatus::Running);
    normal_run.effort = crate::adapters::EffortLevel::Normal;
    let node = agent_run_node(&normal_run);
    assert!(
        !node.label.contains(":xhigh") && !node.label.contains(":max"),
        "Normal run should not have effort suffix, got: {}",
        node.label
    );

    let mut tough_codex = run(2, "coder", RunStatus::Running);
    tough_codex.effort = crate::adapters::EffortLevel::Tough;
    tough_codex.vendor = "codex".to_string();
    let node = agent_run_node(&tough_codex);
    assert!(
        node.label.ends_with(":xhigh"),
        "Tough codex run should end with :xhigh, got: {}",
        node.label
    );

    let mut tough_claude = run(3, "coder", RunStatus::Running);
    tough_claude.effort = crate::adapters::EffortLevel::Tough;
    tough_claude.vendor = "claude".to_string();
    let node = agent_run_node(&tough_claude);
    assert!(
        node.label.ends_with(":max"),
        "Tough claude run should end with :max, got: {}",
        node.label
    );

    let mut tough_gemini = run(4, "coder", RunStatus::Running);
    tough_gemini.effort = crate::adapters::EffortLevel::Tough;
    tough_gemini.vendor = "gemini".to_string();
    let node = agent_run_node(&tough_gemini);
    assert!(
        !node.label.contains(":xhigh") && !node.label.contains(":max"),
        "Tough gemini run should have no effort suffix, got: {}",
        node.label
    );

    let mut low_codex = run(5, "coder", RunStatus::Running);
    low_codex.effort = crate::adapters::EffortLevel::Low;
    low_codex.vendor = "codex".to_string();
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
    state.builder.current_task = Some(1);
    state.builder.pending = vec![2];

    let normal_run = RunRecord {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "gpt-5.2".to_string(),
        vendor: "codex".to_string(),
        window_name: "[Round 1 Coder]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
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
fn final_validation_groups_runs_by_round() {
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
    let stage = nodes
        .iter()
        .find(|n| n.label == "Final Validation")
        .unwrap();
    let mut run_ids = Vec::new();
    collect_run_ids(stage, &mut run_ids);
    assert!(run_ids.contains(&10));
    assert!(run_ids.contains(&20));
    assert_eq!(stage.status, NodeStatus::Running);
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
    let stage = nodes
        .iter()
        .find(|n| n.label == "Simplification")
        .unwrap();
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
fn simplification_skipped_under_yolo_done() {
    let mut state = SessionState::new("test".to_string());
    state.current_phase = Phase::Done;
    state.modes.yolo = true;
    let nodes = build_tree(&state);
    let stage = nodes
        .iter()
        .find(|n| n.label == "Simplification")
        .unwrap();
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
    let stage = nodes
        .iter()
        .find(|n| n.label == "Simplification")
        .unwrap();
    let mut run_ids = Vec::new();
    collect_run_ids(stage, &mut run_ids);
    assert!(run_ids.contains(&30));
    assert!(run_ids.contains(&40));
    assert_eq!(stage.status, NodeStatus::Running);
}
