use crate::state::{Node, NodeKind, NodeStatus, Phase, RunRecord, RunStatus, SessionState};
use std::collections::{BTreeMap, BTreeSet};

pub type NodePath = Vec<usize>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageKey {
    Idea,
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    BuilderLoop,
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
    pub backing_leaf_run_id: Option<u64>,
}

impl VisibleNodeRow {
    pub fn is_expandable(&self) -> bool {
        self.status != NodeStatus::Pending && (self.has_children || self.has_transcript)
    }
}

pub fn build_tree(state: &SessionState) -> Vec<Node> {
    let mut nodes = Vec::new();
    nodes.push(build_idea_node(state));
    nodes.push(build_simple_stage(state, "brainstorm", "Brainstorm"));
    nodes.push(build_review_stage(state, "spec-review", "Spec Review"));
    nodes.push(build_simple_stage(state, "planning", "Planning"));
    nodes.push(build_review_stage(state, "plan-review", "Plan Review"));
    nodes.push(build_simple_stage(state, "sharding", "Sharding"));
    nodes.push(build_builder_stage(state));
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

pub fn collect_all_rows(nodes: &[Node]) -> Vec<VisibleNodeRow> {
    let mut rows = Vec::new();
    flatten_rows(nodes, &mut Vec::new(), &mut Vec::new(), &mut rows, &mut |_| true);
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
    candidates.into_iter().next().map(|candidate| candidate.path)
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

fn build_idea_node(state: &SessionState) -> Node {
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

fn build_simple_stage(state: &SessionState, stage_key: &str, label: &str) -> Node {
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == stage_key)
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

fn build_review_stage(state: &SessionState, stage_key: &str, label: &str) -> Node {
    let runs: Vec<&RunRecord> = state
        .agent_runs
        .iter()
        .filter(|r| r.stage == stage_key)
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

fn build_builder_stage(state: &SessionState) -> Node {
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
    let status = builder_status(state, &coder_runs, &reviewer_runs, &recovery_runs);
    let summary = builder_summary(state, &recovery_runs);
    let mut ordered_task_ids = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
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
    let mut children = Vec::new();
    for task_id in ordered_task_ids {
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
                    label: "Coder".to_string(),
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
        let task_status = if round_nodes.iter().any(|c| c.status == NodeStatus::Running) {
            NodeStatus::Running
        } else if round_nodes.iter().all(|c| c.status == NodeStatus::Done) {
            NodeStatus::Done
        } else if round_nodes.iter().any(|c| c.status == NodeStatus::Failed) {
            NodeStatus::Failed
        } else {
            NodeStatus::Pending
        };
        let label = match state.builder.task_titles.get(&task_id) {
            Some(title) if !title.trim().is_empty() => {
                format!("Task {}: {}", task_id, title.trim())
            }
            _ => format!("Task {}", task_id),
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
    if matches!(state.current_phase, Phase::BuilderRecovery(_)) || !recovery_runs.is_empty() {
        let mut rounds: std::collections::BTreeMap<u32, Vec<&RunRecord>> =
            std::collections::BTreeMap::new();
        for run in &recovery_runs {
            rounds.entry(run.round).or_default().push(*run);
        }
        let mut round_nodes = Vec::new();
        for (round_num, round_runs) in rounds {
            let mut mode_nodes = Vec::new();
            if !round_runs.is_empty() {
                let mut recovery_children = Vec::new();
                for run in &round_runs {
                    recovery_children.push(attempt_run_node(run));
                }
                mode_nodes.push(Node {
                    label: "Recovery".to_string(),
                    kind: NodeKind::Mode,
                    status: rollup_status(&round_runs),
                    summary: String::new(),
                    children: recovery_children,
                    run_id: None,
                    leaf_run_id: None,
                });
            }
            let round_status = rollup_status(&round_runs);
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
        let recovery_status = if matches!(state.current_phase, Phase::BuilderRecovery(_)) {
            if state.agent_error.is_some() {
                NodeStatus::Failed
            } else {
                NodeStatus::Running
            }
        } else if round_nodes.iter().all(|c| c.status == NodeStatus::Done) {
            NodeStatus::Done
        } else if round_nodes.iter().any(|c| c.status == NodeStatus::Failed) {
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
        let done_count = state.builder.done.len();
        children.insert(done_count.min(children.len()), recovery_node);
    }
    Node {
        label: "Builder Loop".to_string(),
        kind: NodeKind::Stage,
        status,
        summary,
        children,
        run_id: None,
        leaf_run_id: None,
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
    Node {
        label: format!("{} · {}", role_label(&run.stage), run.model),
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
        "coder" => "Coder",
        "reviewer" => "Reviewer",
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

fn rollup_status(runs: &[&RunRecord]) -> NodeStatus {
    let latest = latest_attempts(runs);
    if latest.iter().any(|r| r.status == RunStatus::Running) {
        NodeStatus::Running
    } else if latest.is_empty() {
        NodeStatus::Pending
    } else if latest.iter().all(|r| r.status == RunStatus::Done) {
        NodeStatus::Done
    } else {
        NodeStatus::Failed
    }
}

fn run_status_to_node(status: RunStatus) -> NodeStatus {
    match status {
        RunStatus::Running => NodeStatus::Running,
        RunStatus::Done => NodeStatus::Done,
        RunStatus::Failed => NodeStatus::Failed,
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
    let phase_matches = match (stage_key, state.current_phase) {
        ("brainstorm", Phase::BrainstormRunning) => true,
        ("spec-review", Phase::SpecReviewRunning) => true,
        ("spec-review", Phase::SpecReviewPaused) => true,
        ("planning", Phase::PlanningRunning) => true,
        ("plan-review", Phase::PlanReviewRunning) => true,
        ("plan-review", Phase::PlanReviewPaused) => true,
        ("sharding", Phase::ShardingRunning) => true,
        ("coder", Phase::ImplementationRound(_)) => true,
        ("reviewer", Phase::ReviewRound(_)) => true,
        _ => false,
    };
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
            _ => false,
        };
        if is_pending {
            return NodeStatus::Pending;
        }
        return NodeStatus::Done;
    }
    if runs.iter().all(|r| r.status == RunStatus::Done) {
        NodeStatus::Done
    } else if runs.iter().any(|r| r.status == RunStatus::Failed) {
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
        Phase::ImplementationRound(_) | Phase::ReviewRound(_) | Phase::BuilderRecovery(_) => {
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

fn builder_summary(state: &SessionState, recovery_runs: &[&RunRecord]) -> String {
    if matches!(state.current_phase, Phase::BuilderRecovery(_)) {
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
    let total = state.builder.done.len()
        + state.builder.current_task.iter().len()
        + state.builder.pending.len();
    if total == 0 {
        return String::new();
    }
    let done = state.builder.done.len();
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
            let child = node.children.pop().unwrap();
            if child.kind == NodeKind::AgentRun {
                node.leaf_run_id = child.run_id;
            } else {
                node.children = child.children;
                node.leaf_run_id = child.leaf_run_id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        }
    }

    #[test]
    fn test_build_tree_single_stage() {
        let mut state = SessionState::new("test".to_string());
        state.agent_runs.push(run(1, "brainstorm", RunStatus::Done));
        let nodes = build_tree(&state);
        assert_eq!(nodes.len(), 7); // Idea + 6 stages
        let brainstorm = nodes.iter().find(|n| n.label == "Brainstorm").unwrap();
        assert_eq!(brainstorm.kind, NodeKind::Stage);
        assert_eq!(brainstorm.status, NodeStatus::Done);
    }

    #[test]
    fn test_collapse_preserves_task_node() {
        // Task nodes are never absorbed, ensuring they always remain visible
        let mut stage = Node {
            label: "Builder Loop".to_string(),
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
                        label: "Coder".to_string(),
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
            window_name: "[Coder t1 r1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("timeout".to_string()),
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 2,
            model: "claude-opus-4-7".to_string(),
            vendor: "anthropic".to_string(),
            window_name: "[Coder t1 r1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
        });
        let nodes = build_tree(&state);
        let builder = nodes.iter().find(|n| n.label == "Builder Loop").unwrap();
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
        });
        let nodes = build_tree(&state);
        let builder = nodes.iter().find(|n| n.label == "Builder Loop").unwrap();
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
                        label: "Coder".to_string(),
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
        assert_eq!(task.children[0].label, "Coder");
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
            label: "Coder".to_string(),
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
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "coder".to_string(),
            task_id: Some(8),
            round: 2,
            attempt: 1,
            model: "gpt".to_string(),
            vendor: "openai".to_string(),
            window_name: "[Coder 2]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        });

        let nodes = build_tree(&state);
        let rows = collect_all_rows(&nodes);
        let coder_rows = rows
            .into_iter()
            .filter(|row| node_at_path(&nodes, &row.path).is_some_and(|node| node.label == "Coder"))
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
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
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
                .all(|row| node_at_path(&nodes, &row.path).is_none_or(|node| node.label != "Coder"))
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
        });

        let nodes = build_tree(&state);
        let stage_key = node_key_at_path(&nodes, &[2]).expect("spec review key");
        let active = active_stage_paths(&nodes, &state.agent_runs);
        let chosen = active.get(&stage_key).expect("active descendant");
        let chosen_node = node_at_path(&nodes, chosen).expect("chosen node");

        assert_eq!(chosen_node.run_id.or(chosen_node.leaf_run_id), Some(2));
    }
}

pub fn current_node_index(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .position(|n| {
            matches!(
                n.status,
                NodeStatus::Running | NodeStatus::WaitingUser | NodeStatus::Failed
            )
        })
        .or_else(|| nodes.iter().position(|n| n.status == NodeStatus::Pending))
        .or_else(|| {
            nodes
                .iter()
                .rposition(|n| n.status == NodeStatus::Done)
        })
        .unwrap_or(0)
}
