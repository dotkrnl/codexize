use crate::state::{
    Node, NodeKind, NodeStatus, Phase, PipelineItemStatus, RunRecord, RunStatus, SessionState,
};
#[path = "stages.rs"]
mod stages;
use self::stages::{
    build_builder_stage, build_final_validation_stage, build_idea_node, build_review_stage,
    build_simple_stage, build_simplification_stage,
};
use std::collections::{BTreeMap, BTreeSet};

pub type NodePath = Vec<usize>;
type RecoveryRoundRuns<'a> = (Vec<&'a RunRecord>, Vec<&'a RunRecord>, Vec<&'a RunRecord>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageKey {
    Idea,
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    BuilderLoop,
    Simplification,
    FinalValidation,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskKey {
    Task(u32),
    BuilderRecovery,
    Fallback(NodeKind, NodePath),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModeKey {
    Coder,
    Reviewer,
    Recovery,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKeyPart {
    Stage(StageKey),
    Task(TaskKey),
    Round(u32),
    Mode(ModeKey),
    Run(u64),
    Fallback(NodeKind, NodePath),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey {
    parts: Vec<NodeKeyPart>,
}

impl NodeKey {
    pub fn new(parts: Vec<NodeKeyPart>) -> Self {
        Self { parts }
    }

    pub fn ancestors(&self) -> impl Iterator<Item = NodeKey> + '_ {
        (1..self.parts.len()).rev().map(|len| NodeKey {
            parts: self.parts[..len].to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleNodeRow {
    pub depth: usize,
    pub path: NodePath,
    pub key: NodeKey,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub has_children: bool,
    pub has_transcript: bool,
    pub has_body: bool,
    pub backing_leaf_run_id: Option<u64>,
}

impl VisibleNodeRow {
    pub fn is_expandable(&self) -> bool {
        self.status != NodeStatus::Pending
            && (self.has_children || self.has_transcript || self.has_body)
    }
}

pub fn build_tree(state: &SessionState) -> Vec<Node> {
    let mut nodes = vec![
        build_idea_node(state),
        build_simple_stage(state, "brainstorm", "Brainstorm"),
        build_review_stage(state, "spec-review", "Spec Review"),
        build_simple_stage(state, "planning", "Planning"),
        build_review_stage(state, "plan-review", "Plan Review"),
        build_simple_stage(state, "sharding", "Sharding"),
        build_builder_stage(state),
        build_simplification_stage(state),
        build_final_validation_stage(state),
    ];
    for node in &mut nodes {
        collapse_tree(node);
    }
    nodes
}

pub fn node_at_path<'a>(nodes: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let (&first, rest) = path.split_first()?;
    let mut node = nodes.get(first)?;
    for &index in rest {
        node = node.children.get(index)?;
    }
    Some(node)
}

pub fn node_key_at_path(nodes: &[Node], path: &[usize]) -> Option<NodeKey> {
    let mut parts = Vec::new();
    let mut absolute_path = Vec::new();
    let mut current = nodes;

    for (depth, &index) in path.iter().enumerate() {
        absolute_path.push(index);
        let node = current.get(index)?;
        parts.push(node_key_part(node, &absolute_path, depth));
        current = &node.children;
    }

    Some(NodeKey::new(parts))
}

#[cfg(test)]
pub fn collect_all_rows(nodes: &[Node]) -> Vec<VisibleNodeRow> {
    let mut rows = Vec::new();
    flatten_rows(
        nodes,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut rows,
        &mut |_| true,
    );
    rows
}

pub fn flatten_visible_rows(
    nodes: &[Node],
    mut is_expanded: impl FnMut(&VisibleNodeRow) -> bool,
) -> Vec<VisibleNodeRow> {
    let mut rows = Vec::new();
    flatten_rows(
        nodes,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut rows,
        &mut is_expanded,
    );
    rows
}

pub fn active_stage_paths(nodes: &[Node], runs: &[RunRecord]) -> BTreeMap<NodeKey, NodePath> {
    let run_lookup: BTreeMap<u64, &RunRecord> = runs.iter().map(|run| (run.id, run)).collect();
    let mut active = BTreeMap::new();

    for (index, node) in nodes.iter().enumerate() {
        if node.kind != NodeKind::Stage || node.status != NodeStatus::Running {
            continue;
        }
        let path = vec![index];
        let Some(stage_key) = node_key_at_path(nodes, &path) else {
            continue;
        };
        if let Some(best_path) = best_active_descendant_path(nodes, &path, &run_lookup) {
            active.insert(stage_key, best_path);
        }
    }

    active
}

pub fn active_path_keys(nodes: &[Node], runs: &[RunRecord]) -> BTreeSet<NodeKey> {
    let mut keys = BTreeSet::new();
    for path in active_stage_paths(nodes, runs).into_values() {
        for depth in 1..=path.len() {
            if let Some(key) = node_key_at_path(nodes, &path[..depth]) {
                keys.insert(key);
            }
        }
    }
    keys
}

/// Find the deepest node path whose `run_id` or `leaf_run_id` matches `run_id`.
/// Used by progress-follow focus to refocus on the most specific row backing a
/// particular run when the operator has not opted out of automatic following.
pub fn deepest_path_for_run(nodes: &[Node], run_id: u64) -> Option<NodePath> {
    fn walk(nodes: &[Node], path: &mut NodePath, run_id: u64, best: &mut Option<NodePath>) {
        for (index, node) in nodes.iter().enumerate() {
            path.push(index);
            let matches = node.run_id == Some(run_id) || node.leaf_run_id == Some(run_id);
            if matches {
                let replace = best
                    .as_ref()
                    .is_none_or(|existing| existing.len() <= path.len());
                if replace {
                    *best = Some(path.clone());
                }
            }
            walk(&node.children, path, run_id, best);
            path.pop();
        }
    }
    let mut best = None;
    let mut path = Vec::new();
    walk(nodes, &mut path, run_id, &mut best);
    best
}

fn flatten_rows(
    nodes: &[Node],
    parent_path: &mut NodePath,
    parent_parts: &mut Vec<NodeKeyPart>,
    rows: &mut Vec<VisibleNodeRow>,
    is_expanded: &mut impl FnMut(&VisibleNodeRow) -> bool,
) {
    for (index, node) in nodes.iter().enumerate() {
        parent_path.push(index);
        let part = node_key_part(node, parent_path, parent_path.len() - 1);
        parent_parts.push(part);
        let row = VisibleNodeRow {
            depth: parent_path.len() - 1,
            path: parent_path.clone(),
            key: NodeKey::new(parent_parts.clone()),
            kind: node.kind,
            status: node.status,
            has_children: !node.children.is_empty(),
            has_transcript: node.run_id.or(node.leaf_run_id).is_some(),
            has_body: node.label == "Idea",
            backing_leaf_run_id: node.run_id.or(node.leaf_run_id),
        };
        let expanded = is_expanded(&row);
        rows.push(row);
        if expanded {
            flatten_rows(&node.children, parent_path, parent_parts, rows, is_expanded);
        }
        parent_parts.pop();
        parent_path.pop();
    }
}

fn best_active_descendant_path(
    nodes: &[Node],
    stage_path: &[usize],
    run_lookup: &BTreeMap<u64, &RunRecord>,
) -> Option<NodePath> {
    let stage = node_at_path(nodes, stage_path)?;
    let mut candidates = Vec::new();
    collect_leaf_candidates(stage, stage_path, run_lookup, &mut candidates);
    candidates.sort_by(|left, right| right.cmp(left));
    candidates
        .into_iter()
        .next()
        .map(|candidate| candidate.path)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ActiveLeafCandidate {
    priority: ActiveLeafPriority,
    updated_at: chrono::DateTime<chrono::Utc>,
    path: NodePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ActiveLeafPriority {
    Running = 2,
    RecentNonPending = 1,
}

fn collect_leaf_candidates(
    node: &Node,
    path: &[usize],
    run_lookup: &BTreeMap<u64, &RunRecord>,
    out: &mut Vec<ActiveLeafCandidate>,
) {
    let leaf_run_id = node.run_id.or(node.leaf_run_id);
    if node.children.is_empty()
        && node.status != NodeStatus::Pending
        && let Some(run_id) = leaf_run_id
        && let Some(run) = run_lookup.get(&run_id)
    {
        let priority = if run.status == RunStatus::Running {
            ActiveLeafPriority::Running
        } else {
            ActiveLeafPriority::RecentNonPending
        };
        out.push(ActiveLeafCandidate {
            priority,
            updated_at: run.ended_at.unwrap_or(run.started_at),
            path: path.to_vec(),
        });
    }

    for (index, child) in node.children.iter().enumerate() {
        let mut child_path = path.to_vec();
        child_path.push(index);
        collect_leaf_candidates(child, &child_path, run_lookup, out);
    }
}

fn node_key_part(node: &Node, path: &[usize], depth: usize) -> NodeKeyPart {
    if let Some(run_id) = node.run_id {
        return NodeKeyPart::Run(run_id);
    }

    match node.kind {
        NodeKind::Stage => stage_key_for(path.first().copied(), path)
            .map(NodeKeyPart::Stage)
            .unwrap_or_else(|| NodeKeyPart::Fallback(node.kind, path.to_vec())),
        NodeKind::Task => NodeKeyPart::Task(task_key_for(node, path)),
        NodeKind::Round => parse_round(node)
            .map(NodeKeyPart::Round)
            .unwrap_or_else(|| NodeKeyPart::Fallback(node.kind, path.to_vec())),
        NodeKind::Mode => NodeKeyPart::Mode(mode_key_for(node)),
        NodeKind::AgentRun => node
            .leaf_run_id
            .map(NodeKeyPart::Run)
            .unwrap_or_else(|| NodeKeyPart::Fallback(node.kind, path[..=depth].to_vec())),
    }
}

fn stage_key_for(index: Option<usize>, path: &[usize]) -> Option<StageKey> {
    if path.len() != 1 {
        return None;
    }
    match index? {
        0 => Some(StageKey::Idea),
        1 => Some(StageKey::Brainstorm),
        2 => Some(StageKey::SpecReview),
        3 => Some(StageKey::Planning),
        4 => Some(StageKey::PlanReview),
        5 => Some(StageKey::Sharding),
        6 => Some(StageKey::BuilderLoop),
        7 => Some(StageKey::Simplification),
        8 => Some(StageKey::FinalValidation),
        _ => None,
    }
}

fn task_key_for(node: &Node, path: &[usize]) -> TaskKey {
    if node.label == "Builder Recovery" {
        return TaskKey::BuilderRecovery;
    }
    // REVIEWER: pending builder tasks do not carry an intrinsic id in `Node`, so the
    // canonical key currently parses the stable `Task <id>` prefix emitted by build_tree().
    parse_task_id(node)
        .map(TaskKey::Task)
        .unwrap_or_else(|| TaskKey::Fallback(node.kind, path.to_vec()))
}

fn parse_task_id(node: &Node) -> Option<u32> {
    let rest = node.label.strip_prefix("Task ")?;
    let digits = rest.split(':').next()?.trim();
    digits.parse().ok()
}

fn parse_round(node: &Node) -> Option<u32> {
    node.label.strip_prefix("Round ")?.trim().parse().ok()
}

fn mode_key_for(node: &Node) -> ModeKey {
    match node.label.as_str() {
        "Coder" => ModeKey::Coder,
        "Reviewer" => ModeKey::Reviewer,
        "Recovery" => ModeKey::Recovery,
        other => ModeKey::Named(other.to_string()),
    }
}

fn attempt_run_node(run: &RunRecord) -> Node {
    Node {
        label: format!("Attempt {}", run.attempt),
        kind: NodeKind::AgentRun,
        status: run_status_to_node(run.status),
        summary: String::new(),
        children: Vec::new(),
        run_id: Some(run.id),
        leaf_run_id: None,
    }
}

fn agent_run_node(run: &RunRecord) -> Node {
    let effort_suffix = crate::adapters::effort_suffix_from_str(&run.vendor, run.effort);
    let label = format!(
        "{} · {}{}",
        role_label(&run.stage),
        run.model,
        effort_suffix
    );
    Node {
        label,
        kind: NodeKind::AgentRun,
        status: run_status_to_node(run.status),
        summary: String::new(),
        children: Vec::new(),
        run_id: Some(run.id),
        leaf_run_id: None,
    }
}

fn role_label(stage: &str) -> &str {
    match stage {
        "brainstorm" => "Brainstorm",
        "spec-review" => "Reviewer",
        "planning" => "Planning",
        "plan-review" => "Reviewer",
        "sharding" => "Sharding",
        "recovery" => "Recovery",
        "coder" => "Builder",
        "reviewer" => "Reviewer",
        "simplifier" => "Simplifier",
        "final-validation" => "Final Validation",
        _ => stage,
    }
}

fn latest_attempts<'a>(runs: &[&'a RunRecord]) -> Vec<&'a RunRecord> {
    use std::collections::BTreeMap;
    let mut best: BTreeMap<(String, Option<u32>, u32), &'a RunRecord> = BTreeMap::new();
    for run in runs {
        let key = (run.stage.clone(), run.task_id, run.round);
        best.entry(key)
            .and_modify(|existing| {
                if run.attempt > existing.attempt {
                    *existing = *run;
                }
            })
            .or_insert(*run);
    }
    best.into_values().collect()
}

fn is_failed_status(status: NodeStatus) -> bool {
    matches!(status, NodeStatus::Failed | NodeStatus::FailedUnverified)
}

fn rollup_status(runs: &[&RunRecord]) -> NodeStatus {
    let latest = latest_attempts(runs);
    if latest.iter().any(|r| r.status == RunStatus::Running) {
        NodeStatus::Running
    } else if latest.is_empty() {
        NodeStatus::Pending
    } else if latest.iter().all(|r| r.status == RunStatus::Done) {
        NodeStatus::Done
    } else if latest
        .iter()
        .all(|r| r.status == RunStatus::FailedUnverified)
    {
        NodeStatus::FailedUnverified
    } else {
        NodeStatus::Failed
    }
}

fn run_status_to_node(status: RunStatus) -> NodeStatus {
    match status {
        RunStatus::Running => NodeStatus::Running,
        RunStatus::Done => NodeStatus::Done,
        RunStatus::Failed => NodeStatus::Failed,
        RunStatus::FailedUnverified => NodeStatus::FailedUnverified,
    }
}

fn stage_status_from_runs(
    runs: &[&RunRecord],
    state: &SessionState,
    stage_key: &str,
) -> NodeStatus {
    if runs.iter().any(|r| r.status == RunStatus::Running) {
        return NodeStatus::Running;
    }
    let phase_matches = matches!(
        (stage_key, state.current_phase),
        ("brainstorm", Phase::BrainstormRunning)
            | ("spec-review", Phase::SpecReviewRunning)
            | ("spec-review", Phase::SpecReviewPaused)
            | ("planning", Phase::PlanningRunning)
            | ("plan-review", Phase::PlanReviewRunning)
            | ("plan-review", Phase::PlanReviewPaused)
            | ("sharding", Phase::ShardingRunning)
            | ("coder", Phase::ImplementationRound(_))
            | ("reviewer", Phase::ReviewRound(_))
            | ("simplifier", Phase::Simplification(_))
            | ("final-validation", Phase::FinalValidation(_))
    );
    if phase_matches && state.agent_error.is_some() {
        return NodeStatus::Failed;
    }
    if phase_matches && !runs.is_empty() && runs.iter().all(|r| r.status == RunStatus::Done) {
        return match (stage_key, state.current_phase) {
            ("spec-review", Phase::SpecReviewPaused) => NodeStatus::WaitingUser,
            ("plan-review", Phase::PlanReviewPaused) => NodeStatus::WaitingUser,
            _ => NodeStatus::WaitingUser,
        };
    }
    if phase_matches {
        if runs.is_empty() {
            return NodeStatus::Pending;
        }
        return NodeStatus::Running;
    }
    if runs.is_empty() {
        let is_pending = match (stage_key, state.current_phase) {
            ("brainstorm", Phase::IdeaInput) => true,
            ("spec-review", p) => matches!(p, Phase::IdeaInput | Phase::BrainstormRunning),
            ("planning", p) => matches!(
                p,
                Phase::IdeaInput
                    | Phase::BrainstormRunning
                    | Phase::SpecReviewRunning
                    | Phase::SpecReviewPaused
            ),
            ("plan-review", p) => matches!(
                p,
                Phase::IdeaInput
                    | Phase::BrainstormRunning
                    | Phase::SpecReviewRunning
                    | Phase::SpecReviewPaused
                    | Phase::PlanningRunning
            ),
            ("sharding", p) => matches!(
                p,
                Phase::IdeaInput
                    | Phase::BrainstormRunning
                    | Phase::SpecReviewRunning
                    | Phase::SpecReviewPaused
                    | Phase::PlanningRunning
                    | Phase::PlanReviewRunning
                    | Phase::PlanReviewPaused
            ),
            // Simplification is pending across every pre-simplification phase.
            // Done/BlockedNeedsUser without runs falls through to the
            // skip/Done resolution below (yolo or nothing-to-do bypass).
            ("simplifier", p) => !matches!(p, Phase::Done | Phase::BlockedNeedsUser),
            // Final validation is pending across every pre-validation phase
            // (including simplification, which now precedes it).
            ("final-validation", p) => !matches!(p, Phase::Done | Phase::BlockedNeedsUser),
            _ => false,
        };
        if is_pending {
            return NodeStatus::Pending;
        }
        // skip-to-implementation jumped past these intermediate stages without
        // ever running them. Surface that as Skipped (yellow) rather than Done
        // (green) so the user sees a clear "this was bypassed" signal.
        if state.skip_to_impl_kind.is_some()
            && matches!(
                stage_key,
                "spec-review" | "planning" | "plan-review" | "sharding"
            )
        {
            return NodeStatus::Skipped;
        }
        // Yolo and nothing-to-do skip final validation. Simplification is
        // skipped under the same bypasses (yolo / nothing-to-do never enters
        // the simplifier phase). Surface those bypasses explicitly so the
        // operator distinguishes "ran → done" from "bypassed → done".
        if matches!(stage_key, "simplifier" | "final-validation")
            && (state.modes.yolo || state.skip_to_impl_kind.is_some())
        {
            return NodeStatus::Skipped;
        }
        return NodeStatus::Done;
    }
    if runs.iter().all(|r| r.status == RunStatus::Done) {
        NodeStatus::Done
    } else if runs
        .iter()
        .any(|r| matches!(r.status, RunStatus::Failed | RunStatus::FailedUnverified))
    {
        NodeStatus::Failed
    } else {
        NodeStatus::Pending
    }
}

fn stage_summary(
    _state: &SessionState,
    _stage_key: &str,
    label: &str,
    runs: &[&RunRecord],
) -> String {
    if runs.is_empty() {
        return String::new();
    }
    if let Some(last) = runs.last() {
        if runs.len() == 1 && last.status == RunStatus::Done {
            return format!("{} complete", label.to_lowercase());
        }
        if last.status == RunStatus::Running {
            return format!("{} running", label.to_lowercase());
        }
    }
    String::new()
}

fn builder_status(
    state: &SessionState,
    coder_runs: &[&RunRecord],
    reviewer_runs: &[&RunRecord],
    recovery_runs: &[&RunRecord],
) -> NodeStatus {
    if coder_runs.iter().any(|r| r.status == RunStatus::Running)
        || reviewer_runs.iter().any(|r| r.status == RunStatus::Running)
        || recovery_runs.iter().any(|r| r.status == RunStatus::Running)
    {
        return NodeStatus::Running;
    }
    match state.current_phase {
        Phase::ImplementationRound(_)
        | Phase::ReviewRound(_)
        | Phase::BuilderRecovery(_)
        | Phase::BuilderRecoveryPlanReview(_)
        | Phase::BuilderRecoverySharding(_) => {
            if state.agent_error.is_some() {
                NodeStatus::Failed
            } else {
                NodeStatus::Running
            }
        }
        Phase::BlockedNeedsUser => NodeStatus::WaitingUser,
        Phase::Done => NodeStatus::Done,
        _ => NodeStatus::Pending,
    }
}

fn recovery_rounds_for_stage(state: &SessionState, stage: &str) -> BTreeSet<u32> {
    let mut rounds: BTreeSet<u32> = state
        .builder
        .pipeline_items
        .iter()
        .filter(|item| item.stage == stage && item.mode.as_deref() == Some("recovery"))
        .filter_map(|item| item.round)
        .collect();

    let recovery_rounds: BTreeSet<u32> = state
        .agent_runs
        .iter()
        .filter(|run| run.stage == "recovery")
        .map(|run| run.round)
        .collect();
    for run in state.agent_runs.iter().filter(|run| run.stage == stage) {
        // RunRecord has no phase discriminator today; this round-only join is
        // the recovery attribution chokepoint if phase tagging is added later.
        if recovery_rounds.contains(&run.round) {
            rounds.insert(run.round);
        }
    }

    rounds
}

fn builder_summary(state: &SessionState, recovery_runs: &[&RunRecord]) -> String {
    if matches!(
        state.current_phase,
        Phase::BuilderRecovery(_)
            | Phase::BuilderRecoveryPlanReview(_)
            | Phase::BuilderRecoverySharding(_)
    ) {
        return "builder recovery in progress".to_string();
    }
    if !recovery_runs.is_empty()
        && matches!(
            state.current_phase,
            Phase::ImplementationRound(_) | Phase::ReviewRound(_)
        )
    {
        return "builder resumed after recovery".to_string();
    }
    let total = state.builder.done_task_ids().len()
        + state.builder.current_task_id().iter().len()
        + state.builder.pending_task_ids().len();
    if total == 0 {
        return String::new();
    }
    let done = state.builder.done_task_ids().len();
    if done == total {
        return "all tasks complete".to_string();
    }
    format!("{} of {} tasks done", done, total)
}

/// Collapse single-child layers selectively.
/// Only Round and AgentRun nodes may be absorbed by their parent.
/// Stage, Task, and Mode nodes are NEVER absorbed—they always remain visible.
pub fn collapse_tree(node: &mut Node) {
    for child in &mut node.children {
        collapse_tree(child);
    }
    if node.children.len() == 1 {
        let child_kind = node.children[0].kind;
        if matches!(child_kind, NodeKind::Round | NodeKind::AgentRun) {
            let child = node
                .children
                .pop()
                .expect("invariant: len() == 1 before collapsing single-child tree layer");
            if child.kind == NodeKind::AgentRun {
                node.leaf_run_id = child.run_id;
            } else {
                node.children = child.children;
                node.leaf_run_id = child.leaf_run_id;
            }
        }
    }
}

pub fn current_node_index(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .position(|n| {
            matches!(
                n.status,
                NodeStatus::Running
                    | NodeStatus::WaitingUser
                    | NodeStatus::Failed
                    | NodeStatus::FailedUnverified
            )
        })
        .or_else(|| nodes.iter().position(|n| n.status == NodeStatus::Pending))
        .or_else(|| nodes.iter().rposition(|n| n.status == NodeStatus::Done))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "tests_mod.rs"]
mod tests_mod;
