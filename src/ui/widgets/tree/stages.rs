use super::*;

pub(super) fn build_idea_node(state: &SessionState) -> Node {
    let (status, summary) = match state.current_phase {
        Phase::IdeaInput => (NodeStatus::WaitingUser, "waiting for idea".to_string()),
        _ => (NodeStatus::Done, "idea captured".to_string()),
    };
    Node {
        label: "Idea".to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children: Vec::new(),
        run_id: None,
        leaf_run_id: None,
    }
}

pub(super) fn build_simple_stage(state: &SessionState, stage_key: &str, label: &str) -> Node {
    let recovery_rounds = recovery_rounds_for_stage(state, stage_key);
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == stage_key)
        .filter(|r| !recovery_rounds.contains(&r.round))
        .collect();
    let latest = latest_attempts(&runs);
    let status = stage_status_from_runs(&latest, state, stage_key);
    let summary = stage_summary(state, stage_key, label, &latest);
    let mut children = Vec::new();
    for run in runs {
        children.push(agent_run_node(run));
    }
    Node {
        label: label.to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children,
        run_id: None,
        leaf_run_id: None,
    }
}

pub(super) fn build_review_stage(state: &SessionState, stage_key: &str, label: &str) -> Node {
    let recovery_rounds = recovery_rounds_for_stage(state, stage_key);
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == stage_key)
        .filter(|r| !recovery_rounds.contains(&r.round))
        .collect();
    let latest = latest_attempts(&runs);
    let status = stage_status_from_runs(&latest, state, stage_key);
    let summary = stage_summary(state, stage_key, label, &latest);
    let mut rounds: std::collections::BTreeMap<u32, Vec<&RunRecord>> =
        std::collections::BTreeMap::new();
    for run in &runs {
        rounds.entry(run.round).or_default().push(*run);
    }
    let mut children = Vec::new();
    for (round_num, round_runs) in rounds {
        let mut round_children = Vec::new();
        for run in &round_runs {
            round_children.push(agent_run_node(run));
        }
        let round_status = rollup_status(&round_runs);
        children.push(Node {
            label: format!("Round {}", round_num),
            kind: NodeKind::Round,
            status: round_status,
            summary: String::new(),
            children: round_children,
            run_id: None,
            leaf_run_id: None,
        });
    }
    Node {
        label: label.to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children,
        run_id: None,
        leaf_run_id: None,
    }
}

pub(super) fn build_builder_stage(state: &SessionState) -> Node {
    let coder_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "coder")
        .collect();
    let reviewer_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "reviewer")
        .collect();
    let recovery_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "recovery")
        .collect();
    let recovery_pr_rounds = recovery_rounds_for_stage(state, "plan-review");
    let recovery_sharding_rounds = recovery_rounds_for_stage(state, "sharding");
    let recovery_pr_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "plan-review" && recovery_pr_rounds.contains(&r.round))
        .collect();
    let recovery_sharding_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "sharding" && recovery_sharding_rounds.contains(&r.round))
        .collect();
    let status = builder_status(state, &coder_runs, &reviewer_runs, &recovery_runs);
    let summary = builder_summary(state, &recovery_runs);
    let mut ordered_task_ids = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if state.builder.pipeline_items.is_empty() {
        for id in &state.builder.done {
            if seen.insert(*id) {
                ordered_task_ids.push(*id);
            }
        }
        if let Some(id) = state.builder.current_task
            && seen.insert(id)
        {
            ordered_task_ids.push(id);
        }
        for id in &state.builder.pending {
            if seen.insert(*id) {
                ordered_task_ids.push(*id);
            }
        }
    } else {
        for item in state
            .builder
            .pipeline_items
            .iter()
            .filter(|item| item.stage == "coder")
        {
            if let Some(task_id) = item.task_id
                && seen.insert(task_id)
            {
                ordered_task_ids.push(task_id);
            }
        }
    }
    let task_status_by_id = state
        .builder
        .pipeline_items
        .iter()
        .filter(|item| item.stage == "coder")
        .filter_map(|item| item.task_id.map(|task_id| (task_id, item.status)))
        .collect::<BTreeMap<_, _>>();
    let mut children = Vec::new();
    for &task_id in &ordered_task_ids {
        let task_coder: Vec<&RunRecord> = coder_runs
            .iter()
            .filter(|r| r.task_id == Some(task_id))
            .copied()
            .collect();
        let task_reviewer: Vec<&RunRecord> = reviewer_runs
            .iter()
            .filter(|r| r.task_id == Some(task_id))
            .copied()
            .collect();
        let mut rounds: std::collections::BTreeMap<u32, (Vec<&RunRecord>, Vec<&RunRecord>)> =
            std::collections::BTreeMap::new();
        for run in &task_coder {
            rounds.entry(run.round).or_default().0.push(*run);
        }
        for run in &task_reviewer {
            rounds.entry(run.round).or_default().1.push(*run);
        }
        if rounds.is_empty() {
            rounds.insert(state.builder.iteration.max(1), (Vec::new(), Vec::new()));
        }
        let mut round_nodes = Vec::new();
        for (round_num, (c_runs, r_runs)) in rounds {
            let mut mode_nodes = Vec::new();
            let mut combined: Vec<&RunRecord> = Vec::new();
            if !c_runs.is_empty() {
                let mut coder_children = Vec::new();
                for run in &c_runs {
                    coder_children.push(attempt_run_node(run));
                    combined.push(*run);
                }
                mode_nodes.push(Node {
                    label: "Builder".to_string(),
                    kind: NodeKind::Mode,
                    status: rollup_status(&c_runs),
                    summary: String::new(),
                    children: coder_children,
                    run_id: None,
                    leaf_run_id: None,
                });
            }
            if !r_runs.is_empty() {
                let mut reviewer_children = Vec::new();
                for run in &r_runs {
                    reviewer_children.push(attempt_run_node(run));
                    combined.push(*run);
                }
                mode_nodes.push(Node {
                    label: "Reviewer".to_string(),
                    kind: NodeKind::Mode,
                    status: rollup_status(&r_runs),
                    summary: String::new(),
                    children: reviewer_children,
                    run_id: None,
                    leaf_run_id: None,
                });
            }
            let round_status = rollup_status(&combined);
            round_nodes.push(Node {
                label: format!("Round {}", round_num),
                kind: NodeKind::Round,
                status: round_status,
                summary: String::new(),
                children: mode_nodes,
                run_id: None,
                leaf_run_id: None,
            });
        }
        let task_status = match task_status_by_id.get(&task_id).copied() {
            Some(PipelineItemStatus::Running) => NodeStatus::Running,
            Some(PipelineItemStatus::Approved | PipelineItemStatus::Done) => NodeStatus::Done,
            Some(
                PipelineItemStatus::Failed
                | PipelineItemStatus::HumanBlocked
                | PipelineItemStatus::AgentPivot,
            ) => NodeStatus::Failed,
            Some(PipelineItemStatus::Pending | PipelineItemStatus::Revise) => NodeStatus::Pending,
            None => {
                if round_nodes.iter().any(|c| c.status == NodeStatus::Running) {
                    NodeStatus::Running
                } else if round_nodes.iter().all(|c| c.status == NodeStatus::Done) {
                    NodeStatus::Done
                } else if round_nodes.iter().any(|c| is_failed_status(c.status)) {
                    NodeStatus::Failed
                } else {
                    NodeStatus::Pending
                }
            }
        };
        let is_tough = task_coder
            .iter()
            .chain(task_reviewer.iter())
            .any(|r| r.effort == crate::adapters::EffortLevel::Tough);
        let base_label = match state.builder.task_titles.get(&task_id) {
            Some(title) if !title.trim().is_empty() => {
                format!("Task {}: {}", task_id, title.trim())
            }
            _ => format!("Task {}", task_id),
        };
        let label = if is_tough {
            format!("{base_label} [tough]")
        } else {
            base_label
        };
        children.push(Node {
            label,
            kind: NodeKind::Task,
            status: task_status,
            summary: String::new(),
            children: round_nodes,
            run_id: None,
            leaf_run_id: None,
        });
    }
    let in_recovery_phase = matches!(
        state.current_phase,
        Phase::BuilderRecovery(_)
            | Phase::BuilderRecoveryPlanReview(_)
            | Phase::BuilderRecoverySharding(_)
    );
    if in_recovery_phase
        || !recovery_runs.is_empty()
        || !recovery_pr_runs.is_empty()
        || !recovery_sharding_runs.is_empty()
    {
        // Group all recovery-mode runs (recovery agent, recovery plan-review,
        // recovery sharding) by round so each round node shows the full
        // recover→review→shard sub-pipeline.
        let mut rounds: BTreeMap<u32, RecoveryRoundRuns<'_>> = BTreeMap::new();
        for run in &recovery_runs {
            rounds.entry(run.round).or_default().0.push(*run);
        }
        for run in &recovery_pr_runs {
            rounds.entry(run.round).or_default().1.push(*run);
        }
        for run in &recovery_sharding_runs {
            rounds.entry(run.round).or_default().2.push(*run);
        }
        let recovery_anchor_round = rounds.keys().next().copied();
        let mut round_nodes = Vec::new();
        for (round_num, (rec_runs, pr_runs, sh_runs)) in rounds {
            let mut mode_nodes = Vec::new();
            let mut combined: Vec<&RunRecord> = Vec::new();
            for (label, runs_for_mode) in [
                ("Recovery", &rec_runs),
                ("Plan Review", &pr_runs),
                ("Sharding", &sh_runs),
            ] {
                if runs_for_mode.is_empty() {
                    continue;
                }
                let mut children = Vec::new();
                for run in runs_for_mode {
                    children.push(attempt_run_node(run));
                    combined.push(*run);
                }
                mode_nodes.push(Node {
                    label: label.to_string(),
                    kind: NodeKind::Mode,
                    status: rollup_status(runs_for_mode),
                    summary: String::new(),
                    children,
                    run_id: None,
                    leaf_run_id: None,
                });
            }
            let round_status = rollup_status(&combined);
            round_nodes.push(Node {
                label: format!("Round {}", round_num),
                kind: NodeKind::Round,
                status: round_status,
                summary: String::new(),
                children: mode_nodes,
                run_id: None,
                leaf_run_id: None,
            });
        }
        let recovery_status = if in_recovery_phase {
            if state.agent_error.is_some() {
                NodeStatus::Failed
            } else {
                NodeStatus::Running
            }
        } else if round_nodes.iter().all(|c| c.status == NodeStatus::Done) {
            NodeStatus::Done
        } else if round_nodes.iter().any(|c| is_failed_status(c.status)) {
            NodeStatus::Failed
        } else {
            NodeStatus::Pending
        };
        let recovery_node = Node {
            label: "Builder Recovery".to_string(),
            kind: NodeKind::Task,
            status: recovery_status,
            summary: String::new(),
            children: round_nodes,
            run_id: None,
            leaf_run_id: None,
        };
        let target_task_id = state
            .builder
            .recovery_trigger_task_id
            .or_else(|| state.builder.current_task_id());
        if let Some(task_index) = target_task_id.and_then(|trigger_task_id| {
            ordered_task_ids
                .iter()
                .position(|task_id| *task_id == trigger_task_id)
        }) {
            if let Some(task_node) = children.get_mut(task_index) {
                let insert_pos = recovery_anchor_round
                    .and_then(|anchor| {
                        task_node
                            .children
                            .iter()
                            .position(|node| parse_round(node).is_some_and(|round| round > anchor))
                    })
                    .unwrap_or(task_node.children.len());
                task_node.children.insert(insert_pos, recovery_node);
            }
        } else {
            let fallback_pos = state.builder.done_task_ids().len().min(children.len());
            children.insert(fallback_pos, recovery_node);
        }
    }
    Node {
        label: "Loop".to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children,
        run_id: None,
        leaf_run_id: None,
    }
}

pub(super) fn build_simplification_stage(state: &SessionState) -> Node {
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "simplifier")
        .collect();
    let latest = latest_attempts(&runs);
    let status = stage_status_from_runs(&latest, state, "simplifier");
    let summary = stage_summary(state, "simplifier", "Simplification", &latest);
    let mut rounds: BTreeMap<u32, Vec<&RunRecord>> = BTreeMap::new();
    for run in &runs {
        rounds.entry(run.round).or_default().push(*run);
    }
    let mut children = Vec::new();
    for (round_num, round_runs) in rounds {
        let mut round_children = Vec::new();
        for run in &round_runs {
            round_children.push(agent_run_node(run));
        }
        let round_status = rollup_status(&round_runs);
        children.push(Node {
            label: format!("Round {}", round_num),
            kind: NodeKind::Round,
            status: round_status,
            summary: String::new(),
            children: round_children,
            run_id: None,
            leaf_run_id: None,
        });
    }
    Node {
        label: "Simplification".to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children,
        run_id: None,
        leaf_run_id: None,
    }
}

pub(super) fn build_final_validation_stage(state: &SessionState) -> Node {
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "final-validation")
        .collect();
    let latest = latest_attempts(&runs);
    let status = stage_status_from_runs(&latest, state, "final-validation");
    let summary = stage_summary(state, "final-validation", "Final Validation", &latest);
    let mut rounds: BTreeMap<u32, Vec<&RunRecord>> = BTreeMap::new();
    for run in &runs {
        rounds.entry(run.round).or_default().push(*run);
    }
    let mut children = Vec::new();
    for (round_num, round_runs) in rounds {
        let mut round_children = Vec::new();
        for run in &round_runs {
            round_children.push(agent_run_node(run));
        }
        let round_status = rollup_status(&round_runs);
        children.push(Node {
            label: format!("Round {}", round_num),
            kind: NodeKind::Round,
            status: round_status,
            summary: String::new(),
            children: round_children,
            run_id: None,
            leaf_run_id: None,
        });
    }
    Node {
        label: "Final Validation".to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children,
        run_id: None,
        leaf_run_id: None,
    }
}
