use super::*;
use itertools::Itertools;
use std::collections::BTreeSet;
fn node(
    label: impl Into<String>,
    kind: NodeKind,
    status: NodeStatus,
    summary: impl Into<String>,
    children: Vec<Node>,
) -> Node {
    Node {
        label: label.into(),
        kind,
        status,
        summary: summary.into(),
        children,
        run_id: None,
        leaf_run_id: None,
    }
}

fn group_runs_into_round_nodes(runs: &[&RunRecord]) -> Vec<Node> {
    let mut rounds: BTreeMap<u32, Vec<&RunRecord>> = BTreeMap::new();
    for run in runs {
        rounds.entry(run.round).or_default().push(*run);
    }
    rounds
        .into_iter()
        .map(|(round_num, round_runs)| {
            node(
                format!("Round {}", round_num),
                NodeKind::Round,
                rollup_status(&round_runs),
                "",
                round_runs.iter().map(|r| agent_run_node(r)).collect(),
            )
        })
        .collect()
}
/// Boundary rounds — the rounds at which final-validation has run, sorted
/// ascending and deduplicated. Iteration N's round range is bounded above
/// by `iteration_boundaries(state)[N - 1]` (when present); when absent the
/// iteration is "open" and accepts every round from its start onward.
fn iteration_boundaries(state: &SessionState) -> Vec<u32> {
    state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "final-validation")
        .map(|r| r.round)
        .sorted()
        .dedup()
        .collect()
}
/// Inclusive (start, optional end) round range for the given outer
/// iteration. End is `None` while the iteration's closing FV hasn't run,
/// which means the "open" iteration absorbs every round from `start`
/// onward.
pub(super) fn iteration_round_range(state: &SessionState, iteration: u32) -> (u32, Option<u32>) {
    let bounds = iteration_boundaries(state);
    let start = if iteration <= 1 {
        1
    } else {
        bounds
            .get((iteration - 2) as usize)
            .map(|round| round + 1)
            .unwrap_or(1)
    };
    let end = bounds.get((iteration - 1) as usize).copied();
    (start, end)
}
pub(super) fn round_in_iteration(state: &SessionState, iteration: u32, round: u32) -> bool {
    let (start, end) = iteration_round_range(state, iteration);
    round >= start && end.is_none_or(|last| round <= last)
}
/// Total number of iteration trios the dashboard should emit. Equal to
/// the largest iteration recorded on a pipeline item (validator-created
/// future tasks bump this even before they've started running) OR the
/// number of distinct final-validation rounds observed (so a session
/// where FV ran twice without yet bumping pipeline iteration still gets
/// two trios). Clamped to at least 1 so a fresh session still gets one
/// Loop trio.
pub(super) fn total_iterations(state: &SessionState) -> u32 {
    let max_pipeline = state
        .builder
        .pipeline_items
        .iter()
        .map(|item| item.iteration)
        .max()
        .unwrap_or(1);
    let fv_iterations = iteration_boundaries(state).len() as u32;
    max_pipeline.max(fv_iterations).max(1)
}
/// Decorate a stage label with " · iteration N" when N >= 2 so the
/// label-based StageKey lookup can recover the iteration index. Iteration 1
/// keeps the bare label so existing tests and snapshots stay stable.
pub(super) fn iteration_label(base: &str, iteration: u32) -> String {
    if iteration <= 1 {
        base.to_string()
    } else {
        format!("{base} · iteration {iteration}")
    }
}
/// Task ids that belong to the requested iteration (in pipeline order).
pub(super) fn task_ids_for_iteration(state: &SessionState, iteration: u32) -> Vec<u32> {
    state
        .builder
        .pipeline_items
        .iter()
        .filter(|item| item.stage == "coder" && item.iteration == iteration)
        .filter_map(|item| item.task_id)
        .unique()
        .collect()
}
pub(super) fn build_idea_node(state: &SessionState) -> Node {
    let (status, summary) = match state.current_phase {
        Phase::IdeaInput => (NodeStatus::WaitingUser, "waiting for idea".to_string()),
        _ => (NodeStatus::Done, "idea captured".to_string()),
    };
    node("Idea", NodeKind::Stage, status, summary, Vec::new())
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
    let children = runs.iter().map(|run| agent_run_node(run)).collect();
    node(label, NodeKind::Stage, status, summary, children)
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
    let children = group_runs_into_round_nodes(&runs);
    node(label, NodeKind::Stage, status, summary, children)
}
pub(super) fn build_builder_stage(state: &SessionState, iteration: u32) -> Node {
    let in_iteration_round = |round: u32| round_in_iteration(state, iteration, round);
    let coder_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "coder" && in_iteration_round(r.round))
        .collect();
    let reviewer_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "reviewer" && in_iteration_round(r.round))
        .collect();
    let recovery_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == "recovery" && in_iteration_round(r.round))
        .collect();
    let recovery_pr_rounds = recovery_rounds_for_stage(state, "plan-review");
    let recovery_sharding_rounds = recovery_rounds_for_stage(state, "sharding");
    let recovery_pr_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| {
            r.stage == "plan-review"
                && recovery_pr_rounds.contains(&r.round)
                && in_iteration_round(r.round)
        })
        .collect();
    let recovery_sharding_runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| {
            r.stage == "sharding"
                && recovery_sharding_rounds.contains(&r.round)
                && in_iteration_round(r.round)
        })
        .collect();
    let is_open_iteration = iteration_round_range(state, iteration).1.is_none();
    let status = if is_open_iteration {
        builder_status(state, &coder_runs, &reviewer_runs, &recovery_runs)
    } else {
        per_iteration_terminal_status(
            &coder_runs
                .iter()
                .chain(reviewer_runs.iter())
                .chain(recovery_runs.iter())
                .copied()
                .collect::<Vec<_>>(),
        )
    };
    let summary = if iteration <= 1 {
        builder_summary(state, &recovery_runs)
    } else {
        String::new()
    };
    let ordered_task_ids = task_ids_for_iteration(state, iteration);
    let task_id_set: BTreeSet<u32> = ordered_task_ids.iter().copied().collect();
    let task_status_by_id: BTreeMap<u32, PipelineItemStatus> = state
        .builder
        .pipeline_items
        .iter()
        .filter(|item| item.stage == "coder")
        .filter_map(|item| {
            let id = item.task_id?;
            task_id_set.contains(&id).then_some((id, item.status))
        })
        .collect();
    // Group runs into chronological round segments. A "segment" is one
    // contiguous streak of same-(task_id, round) runs in global id order,
    // with no run of a different (task_id, round) interleaved between them.
    // Two non-contiguous occurrences of the same (task, round) — e.g. a
    // palette_retry that re-enters round 8 after rounds 10-22 already
    // completed — produce sibling Round nodes so the rendered tree stays
    // chronological.
    type RoundSegment<'a> = (u32, Vec<&'a RunRecord>, Vec<&'a RunRecord>);
    let mut segments_by_task: BTreeMap<u32, Vec<RoundSegment<'_>>> = BTreeMap::new();
    let ordered_task_id_set: BTreeSet<u32> = ordered_task_ids.iter().copied().collect();
    let mut chronological: Vec<(&RunRecord, bool)> = coder_runs
        .iter()
        .map(|r| (*r, true))
        .chain(reviewer_runs.iter().map(|r| (*r, false)))
        .filter(|(r, _)| r.task_id.is_some_and(|t| ordered_task_id_set.contains(&t)))
        .collect();
    chronological.sort_by_key(|(r, _)| r.id);
    let mut last_key: Option<(u32, u32)> = None;
    for (run, is_coder) in chronological {
        let task_id = run.task_id.expect("filtered above");
        let key = (task_id, run.round);
        let task_segments = segments_by_task.entry(task_id).or_default();
        let same_streak =
            last_key == Some(key) && task_segments.last().is_some_and(|seg| seg.0 == run.round);
        if !same_streak {
            task_segments.push((run.round, Vec::new(), Vec::new()));
        }
        let last_seg = task_segments
            .last_mut()
            .expect("segment was just pushed if needed");
        if is_coder {
            last_seg.1.push(run);
        } else {
            last_seg.2.push(run);
        }
        last_key = Some(key);
    }
    let mut children = Vec::new();
    for &task_id in &ordered_task_ids {
        let is_tough = coder_runs
            .iter()
            .filter(|r| r.task_id == Some(task_id))
            .chain(reviewer_runs.iter().filter(|r| r.task_id == Some(task_id)))
            .any(|r| r.effort == crate::adapters::EffortLevel::Tough);
        let mut segments: Vec<RoundSegment<'_>> =
            segments_by_task.remove(&task_id).unwrap_or_default();
        if segments.is_empty() {
            // Empty placeholder (no runs yet) — keeps an empty Round so the
            // task pipeline still renders before its first run lands.
            segments.push((state.builder.iteration.max(1), Vec::new(), Vec::new()));
        }
        let mut round_nodes = Vec::new();
        for (round_num, c_runs, r_runs) in segments {
            let mut combined: Vec<&RunRecord> = Vec::new();
            let mut mode_nodes = Vec::new();
            for (label, runs) in [("Builder", &c_runs), ("Reviewer", &r_runs)] {
                if runs.is_empty() {
                    continue;
                }
                combined.extend(runs.iter().copied());
                mode_nodes.push(node(
                    label,
                    NodeKind::Mode,
                    rollup_status(runs),
                    "",
                    runs.iter().map(|r| attempt_run_node(r)).collect(),
                ));
            }
            round_nodes.push(node(
                format!("Round {}", round_num),
                NodeKind::Round,
                rollup_status(&combined),
                "",
                mode_nodes,
            ));
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
        children.push(node(label, NodeKind::Task, task_status, "", round_nodes));
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
            let mut combined: Vec<&RunRecord> = Vec::new();
            let mut mode_nodes = Vec::new();
            for (label, runs_for_mode) in [
                ("Recovery", &rec_runs),
                ("Plan Review", &pr_runs),
                ("Sharding", &sh_runs),
            ] {
                if runs_for_mode.is_empty() {
                    continue;
                }
                combined.extend(runs_for_mode.iter().copied());
                mode_nodes.push(node(
                    label,
                    NodeKind::Mode,
                    rollup_status(runs_for_mode),
                    "",
                    runs_for_mode.iter().map(|r| attempt_run_node(r)).collect(),
                ));
            }
            round_nodes.push(node(
                format!("Round {}", round_num),
                NodeKind::Round,
                rollup_status(&combined),
                "",
                mode_nodes,
            ));
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
        let recovery_node = node(
            "Builder Recovery",
            NodeKind::Task,
            recovery_status,
            "",
            round_nodes,
        );
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
    node(
        iteration_label("Loop", iteration),
        NodeKind::Stage,
        status,
        summary,
        children,
    )
}
pub(super) fn build_simplification_stage(state: &SessionState, iteration: u32) -> Node {
    build_per_iteration_stage(state, iteration, "simplifier", "Simplification")
}
pub(super) fn build_final_validation_stage(state: &SessionState, iteration: u32) -> Node {
    build_per_iteration_stage(state, iteration, "final-validation", "Final Validation")
}
pub(super) fn build_dreaming_stage(state: &SessionState) -> Node {
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|run| run.stage == "dreaming")
        .collect();
    let latest = latest_attempts(&runs);
    let status = if latest.iter().any(|run| run.status == RunStatus::Running) {
        NodeStatus::Running
    } else {
        match state.current_phase {
            Phase::DreamingPending => NodeStatus::WaitingUser,
            Phase::Dreaming(_) => {
                if state.agent_error.is_some() {
                    NodeStatus::Failed
                } else if latest.is_empty() {
                    NodeStatus::Pending
                } else {
                    rollup_status(&latest)
                }
            }
            _ => match state
                .dreaming_decision
                .as_ref()
                .map(|decision| decision.kind)
            {
                Some(
                    crate::state::DreamingDecisionKind::ValidatorSkipped
                    | crate::state::DreamingDecisionKind::OperatorSkipped,
                ) => NodeStatus::Skipped,
                Some(crate::state::DreamingDecisionKind::OperatorAccepted) => {
                    per_iteration_terminal_status(&latest)
                }
                Some(crate::state::DreamingDecisionKind::Pending) => NodeStatus::WaitingUser,
                None if state.current_phase == Phase::Done => NodeStatus::Skipped,
                None => NodeStatus::Pending,
            },
        }
    };
    let children = group_runs_into_round_nodes(&runs);
    node("Dreaming", NodeKind::Stage, status, "", children)
}
/// Shared scaffolding for the iteration-scoped trio's tail two stages.
/// Filters runs to this iteration, derives stage status/summary (delegating
/// to the global phase machinery only for iteration 1, since later trios
/// must report from their own runs), and groups the runs into Round nodes.
fn build_per_iteration_stage(
    state: &SessionState,
    iteration: u32,
    stage_key: &str,
    label: &str,
) -> Node {
    let in_iteration_round = |round: u32| round_in_iteration(state, iteration, round);
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == stage_key && in_iteration_round(r.round))
        .collect();
    let latest = latest_attempts(&runs);
    let (status, summary) = if iteration <= 1 {
        (
            stage_status_from_runs(&latest, state, stage_key),
            stage_summary(state, stage_key, label, &latest),
        )
    } else {
        (per_iteration_terminal_status(&latest), String::new())
    };
    let children = group_runs_into_round_nodes(&runs);
    node(
        iteration_label(label, iteration),
        NodeKind::Stage,
        status,
        summary,
        children,
    )
}
/// Roll up the per-iteration Simplification / Final Validation status when
/// iteration > 1: only the *current* iteration owns the global phase
/// machinery (`Phase::Simplification`/`Phase::FinalValidation`), so older
/// trios derive their status from the latest attempts of their own runs.
fn per_iteration_terminal_status(latest: &[&RunRecord]) -> NodeStatus {
    if latest.iter().any(|r| r.status == RunStatus::Running) {
        NodeStatus::Running
    } else if latest.is_empty() {
        NodeStatus::Pending
    } else if latest.iter().all(|r| r.status == RunStatus::Done) {
        NodeStatus::Done
    } else if latest
        .iter()
        .any(|r| matches!(r.status, RunStatus::Failed | RunStatus::FailedUnverified))
    {
        NodeStatus::Failed
    } else {
        NodeStatus::Pending
    }
}
