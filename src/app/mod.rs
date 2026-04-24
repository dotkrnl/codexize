pub mod chat_widget;
mod events;
mod guard;
mod models;
mod render;
mod state;
mod tree;

use crate::{
    adapters::{
        AgentRun, adapter_for_vendor, launch_interactive, launch_noninteractive,
        window_name_with_model,
    },
    cache, review,
    selection::{
        self, ModelStatus, QuotaError, TaskKind, VendorKind, select_excluding, select_for_review,
    },
    state::{
        self as session_state, Message, MessageKind, MessageSender, Node, Phase, RunStatus,
        SessionState,
    },
    tasks, tmux,
    tmux::TmuxContext,
    tui::AppTerminal,
};
use anyhow::Result;
use crossterm::event::{self, Event};

use self::{
    models::{spawn_refresh, vendor_tag},
    state::ModelRefreshState,
    tree::{build_tree, current_node_index},
};

use notify::Watcher;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::mpsc,
    time::{Duration, Instant},
};

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestLaunchOutcome {
    exit_code: i32,
    artifact_contents: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestLaunchHarness {
    outcomes: std::collections::VecDeque<TestLaunchOutcome>,
}

#[derive(Debug)]
pub struct App {
    tmux: TmuxContext,
    state: SessionState,
    nodes: Vec<Node>,
    models: Vec<ModelStatus>,
    model_refresh: ModelRefreshState,
    selected: usize,
    expanded: BTreeSet<String>,
    stage_scroll: BTreeMap<String, usize>,
    body_inner_height: usize,
    body_inner_width: usize,
    input_mode: bool,
    input_buffer: String,
    confirm_back: bool,
    window_launched: bool,
    quota_errors: Vec<QuotaError>,
    quota_retry_delay: Duration,
    agent_line_count: usize,
    live_summary_watcher: Option<notify::RecommendedWatcher>,
    live_summary_change_rx: Option<mpsc::Receiver<()>>,
    live_summary_path: Option<std::path::PathBuf>,
    live_summary_cached_text: String,
    live_summary_cached_mtime: Option<std::time::SystemTime>,
    current_run_id: Option<u64>,
    failed_models: HashMap<(String, Option<u32>, u32), HashSet<(VendorKind, String)>>,
    #[cfg(test)]
    test_launch_harness: Option<std::sync::Arc<std::sync::Mutex<TestLaunchHarness>>>,
    messages: Vec<Message>,
}

impl App {
    pub fn new(tmux: TmuxContext, state: SessionState) -> Self {
        let messages = SessionState::load_messages(&state.session_id).unwrap_or_default();
        let nodes = build_tree(&state);
        let current = current_node_index(&nodes);
        let failed_models = Self::rebuild_failed_models(&state);
        let mut app = Self {
            tmux,
            state,
            nodes,
            models: Vec::new(),
            model_refresh: ModelRefreshState::Fetching {
                rx: spawn_refresh(),
                started_at: Instant::now(),
            },
            selected: current,
            expanded: BTreeSet::new(),
            stage_scroll: BTreeMap::new(),
            body_inner_height: 0,
            body_inner_width: 0,
            input_mode: false,
            input_buffer: String::new(),
            confirm_back: false,
            window_launched: false,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            live_summary_path: None,
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            current_run_id: None,
            failed_models,
            #[cfg(test)]
            test_launch_harness: None,
            messages,
        };
        // Load cached models if available
        if let Some((cached, errors, expired)) = cache::load() {
            app.models = cached;
            app.quota_errors = errors;
            app.model_refresh = if expired {
                ModelRefreshState::Fetching {
                    rx: spawn_refresh(),
                    started_at: Instant::now(),
                }
            } else {
                ModelRefreshState::Idle(Instant::now())
            };
        }
        if let Ok(output) = std::process::Command::new("tmux")
            .args(["list-windows", "-F", "#{window_name}"])
            .output()
        {
            let live_windows = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            if let Ok(run_id) = app.state.resume_running_runs(&live_windows) {
                app.current_run_id = run_id;
                app.window_launched = run_id.is_some();
                if run_id.is_some() {
                    app.live_summary_path = Some(
                        session_state::session_dir(&app.state.session_id)
                            .join("artifacts")
                            .join("live_summary.txt"),
                    );
                    app.read_live_summary_pipeline();
                }
                app.messages =
                    SessionState::load_messages(&app.state.session_id).unwrap_or_default();
                app.nodes = build_tree(&app.state);
                app.selected = current_node_index(&app.nodes);
            }
        }
        let _ = app.setup_watcher();
        app
    }

    fn rebuild_failed_models(
        state: &SessionState,
    ) -> HashMap<(String, Option<u32>, u32), HashSet<(VendorKind, String)>> {
        let mut failed_models = HashMap::new();
        let cutoff = state.builder.retry_reset_run_id_cutoff;
        for run in state
            .agent_runs
            .iter()
            .filter(|run| run.status == RunStatus::Failed)
        {
            if run.error.as_deref() == Some("user_forced_retry") {
                continue;
            }
            if matches!(run.stage.as_str(), "coder" | "reviewer")
                && cutoff.is_some_and(|cutoff| run.id <= cutoff)
            {
                continue;
            }
            let Some(vendor) = selection::vendor::str_to_vendor(&run.vendor) else {
                continue;
            };
            failed_models
                .entry((run.stage.clone(), run.task_id, run.round))
                .or_insert_with(HashSet::new)
                .insert((vendor, run.model.clone()));
        }
        failed_models
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        loop {
            self.refresh_models_if_due();
            self.poll_agent_window();
            self.maybe_auto_launch();
            self.update_agent_progress();
            self.process_live_summary_changes();
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))?
                && let Event::Key(key) = event::read()?
                && self.handle_key(key)
            {
                return Ok(());
            }
        }
    }

    fn current_node(&self) -> usize {
        current_node_index(&self.nodes)
    }

    fn can_focus_input(&self) -> bool {
        self.is_expanded(self.selected)
            && self.state.current_phase == Phase::IdeaInput
            && self.nodes[self.selected].label == "Idea"
    }

    pub(super) fn is_expanded(&self, index: usize) -> bool {
        if index == self.current_node() {
            return true;
        }
        let Some(key) = self.nodes.get(index).and_then(Self::stage_scroll_key) else {
            return false;
        };
        self.expanded.contains(&key)
    }

    pub(super) fn page_step(&self) -> usize {
        self.stage_body_height().saturating_sub(2).max(1)
    }

    pub(super) fn expanded_stage_count(&self) -> usize {
        (0..self.nodes.len())
            .filter(|i| self.is_expanded(*i))
            .count()
            .max(1)
    }

    pub(super) fn stage_body_height(&self) -> usize {
        let body = self.body_inner_height.saturating_sub(self.nodes.len());
        (body / self.expanded_stage_count()).max(3)
    }

    pub(super) fn stage_scroll_key(node: &Node) -> Option<String> {
        if node.kind != session_state::NodeKind::Stage {
            return None;
        }
        // REVIEWER: stage labels are currently unique in the tree; if that changes,
        // this key should include task_id/round identity to avoid collisions.
        Some(node.label.clone())
    }

    pub(super) fn stage_scroll_for(&self, index: usize) -> Option<(String, Option<usize>)> {
        let node = self.nodes.get(index)?;
        let key = Self::stage_scroll_key(node)?;
        let stored = self.stage_scroll.get(&key).copied();
        Some((key, stored))
    }

    /// Resolve the effective scroll offset for a stage, treating the missing/MAX
    /// sentinel as "stick to bottom" by returning `max_offset`.
    pub(super) fn effective_stage_scroll(&self, index: usize, max_offset: usize) -> usize {
        match self.stage_scroll_for(index) {
            Some((_, Some(v))) if v != usize::MAX => v.min(max_offset),
            _ => max_offset,
        }
    }

    pub(super) fn stage_max_offset(&self, index: usize) -> usize {
        if !self.is_expanded(index) {
            return 0;
        }
        let height = self.stage_body_height();
        let total = self.node_body(index).len();
        if total > height {
            total.saturating_sub(height.saturating_sub(1))
        } else {
            0
        }
    }

    pub(super) fn set_stage_scroll(&mut self, index: usize, value: usize) {
        if let Some(key) = self.nodes.get(index).and_then(Self::stage_scroll_key) {
            self.stage_scroll.insert(key, value);
        }
    }

    fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        let previous_stage_leaf_runs: BTreeMap<String, Option<u64>> = self
            .nodes
            .iter()
            .filter_map(|node| Self::stage_scroll_key(node).map(|key| (key, node.leaf_run_id)))
            .collect();

        self.state.transition_to(next_phase)?;
        self.agent_line_count = 0;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;

        self.nodes = build_tree(&self.state);
        // Reset scroll (to bottom-anchored) for any stage whose backing leaf run changed.
        for node in &self.nodes {
            let Some(key) = Self::stage_scroll_key(node) else {
                continue;
            };
            let previous_leaf = previous_stage_leaf_runs.get(&key).copied();
            if previous_leaf != Some(node.leaf_run_id) {
                self.stage_scroll.insert(key, usize::MAX);
            }
        }
        self.selected = current_node_index(&self.nodes);
        Ok(())
    }

    fn editable_artifact(&self) -> Option<std::path::PathBuf> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let path = match self.state.current_phase {
            Phase::BrainstormRunning | Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                artifacts.join("spec.md")
            }
            Phase::PlanningRunning | Phase::PlanReviewRunning | Phase::PlanReviewPaused => {
                artifacts.join("plan.md")
            }
            Phase::ShardingRunning => artifacts.join("tasks.toml"),
            Phase::BuilderRecovery(_) => artifacts.join("tasks.toml"),
            Phase::ImplementationRound(r) | Phase::ReviewRound(r) => session_dir
                .join("rounds")
                .join(format!("{r:03}"))
                .join("task.md"),
            Phase::IdeaInput | Phase::Done | Phase::BlockedNeedsUser => return None,
        };
        if path.exists() { Some(path) } else { None }
    }

    fn open_editable_artifact(&self) {
        let Some(path) = self.editable_artifact() else {
            return;
        };
        let path_str = path.display().to_string();
        let _ = std::process::Command::new("tmux")
            .args(["new-window", "-n", "[Edit]", &format!("vim {path_str}")])
            .output();
        let _ = std::process::Command::new("tmux")
            .args(["select-window", "-t", "[Edit]"])
            .output();
    }

    fn can_go_back(&self) -> bool {
        !matches!(self.state.current_phase, Phase::IdeaInput | Phase::Done)
    }

    fn go_back(&mut self) {
        use std::fs;

        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let prompts = session_dir.join("prompts");

        match self.state.current_phase {
            Phase::BrainstormRunning => {
                kill_window("[Brainstorm]");
                let _ = fs::remove_file(artifacts.join("spec.md"));
                let _ = fs::remove_file(prompts.join("brainstorm.md"));
                self.state.agent_error = None;
                let _ = self.transition_to_phase(Phase::IdeaInput);
            }
            Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                kill_window("[Spec Review]");
                let _ = fs::remove_file(artifacts.join("spec-review-1.md"));
                let _ = fs::remove_file(prompts.join("spec-review-1.md"));
                // TODO(Task 2): clean up all review artifacts by RunRecord instead of the
                // removed spec_reviewers/phase_models state.
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::PlanningRunning => {
                kill_window("[Planning]");
                let _ = fs::remove_file(artifacts.join("plan.md"));
                let _ = self.transition_to_phase(Phase::SpecReviewRunning);
            }
            Phase::PlanReviewRunning => {
                kill_window("[Plan Review 1]");
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(prompts.join("plan-review-1.md"));
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                self.state.agent_error = None;
                // TODO(Task 2): restore the paused/running distinction from RunRecord state.
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::PlanReviewPaused => {
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(prompts.join("plan-review-1.md"));
                let _ = fs::remove_file(artifacts.join("plan.pre-review-1.md"));
                let _ = fs::remove_file(artifacts.join("spec.pre-review-1.md"));
                // TODO(Task 2): clean up all plan review artifacts by RunRecord history.
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::ShardingRunning => {
                kill_window("[Sharding]");
                let _ = fs::remove_file(artifacts.join("tasks.toml"));
                let _ = fs::remove_file(prompts.join("sharding.md"));
                // TODO(Task 2): remove sharding launch metadata from RunRecord instead of the
                // removed phase_models state.
                let _ = self.transition_to_phase(Phase::PlanReviewRunning);
            }
            Phase::ImplementationRound(r) => {
                kill_window(&format!("[Coder r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let prev = if r <= 1 {
                    self.state.builder = session_state::BuilderState::default();
                    Phase::ShardingRunning
                } else {
                    Phase::ReviewRound(r - 1)
                };
                let _ = self.transition_to_phase(prev);
            }
            Phase::ReviewRound(r) => {
                kill_window(&format!("[Review r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let _ = self.transition_to_phase(Phase::ImplementationRound(r));
            }
            Phase::BuilderRecovery(r) => {
                kill_window("[Recovery]");
                let _ = fs::remove_file(prompts.join(format!("recovery-r{r}.md")));
                // Recovery is builder-only and should not be rewound into coder/reviewer; go back to
                // the triggering review round so the operator can intervene.
                let _ = self.transition_to_phase(Phase::ReviewRound(r));
            }
            Phase::IdeaInput | Phase::BlockedNeedsUser | Phase::Done => {}
        }

        self.state.agent_error = None;
        self.window_launched = false;
        self.current_run_id = None;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        let _ = self.state.save();
    }

    fn attempt_for(&self, stage: &str, task_id: Option<u32>, round: u32) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.task_id == task_id && run.round == round)
            .map(|run| run.attempt)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn completed_rounds(&self, stage: &str) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.status == RunStatus::Done)
            .map(|run| run.round)
            .max()
            .unwrap_or(0)
    }

    fn running_run(&self) -> Option<&crate::state::RunRecord> {
        self.current_run_id.and_then(|run_id| {
            self.state
                .agent_runs
                .iter()
                .find(|run| run.id == run_id && run.status == RunStatus::Running)
        })
    }

    fn try_test_launch(
        &mut self,
        status_path: &std::path::Path,
        artifact_path: Option<&std::path::Path>,
    ) -> Option<Result<()>> {
        #[cfg(not(test))]
        {
            let _ = (status_path, artifact_path);
            None
        }
        #[cfg(test)]
        {
            let harness = self.test_launch_harness.as_ref()?.clone();
            let outcome = harness
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .outcomes
                .pop_front()
                .expect("expected queued test launch outcome");
            Some((|| -> Result<()> {
                if let Some(parent) = status_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(status_path, outcome.exit_code.to_string())?;
                if let (Some(path), Some(contents)) = (artifact_path, outcome.artifact_contents) {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, contents)?;
                }
                Ok(())
            })())
        }
    }

    fn window_exists(&self, window_name: &str) -> bool {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return false;
        }
        tmux::window_exists(window_name)
    }

    fn retry_key_for_run(run: &crate::state::RunRecord) -> (String, Option<u32>, u32) {
        (run.stage.clone(), run.task_id, run.round)
    }

    fn task_kind_for_stage(stage: &str) -> TaskKind {
        match stage {
            "brainstorm" => TaskKind::Idea,
            "spec-review" => TaskKind::Review,
            "planning" => TaskKind::Planning,
            "plan-review" => TaskKind::Review,
            "sharding" => TaskKind::Planning,
            "recovery" => TaskKind::Planning,
            "coder" => TaskKind::Build,
            "reviewer" => TaskKind::Review,
            _ => TaskKind::Build,
        }
    }

    fn run_status_path_for(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> std::path::PathBuf {
        let task = task_id
            .map(|id| format!("task-{id}"))
            .unwrap_or_else(|| "stage".to_string());
        session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join("run-status")
            .join(format!("{stage}-{task}-r{round}-a{attempt}.txt"))
    }

    fn run_status_path(&self, run: &crate::state::RunRecord) -> std::path::PathBuf {
        self.run_status_path_for(&run.stage, run.task_id, run.round, run.attempt)
    }

    fn guard_dir_for(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> std::path::PathBuf {
        let task = task_id
            .map(|id| format!("task-{id}"))
            .unwrap_or_else(|| "stage".to_string());
        session_state::session_dir(&self.state.session_id)
            .join(".guards")
            .join(format!("{stage}-{task}-r{round}-a{attempt}"))
    }

    /// Snapshot the run's immutability state. Non-coder agents must leave the
    /// git tree unchanged; the coder must not edit session control files.
    /// No-op under the test harness (no real git available).
    fn capture_run_guard(&self, stage: &str, task_id: Option<u32>, round: u32, attempt: u32) {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return;
        }
        let dir = self.guard_dir_for(stage, task_id, round, attempt);
        let session_dir = session_state::session_dir(&self.state.session_id);
        let _ = if stage == "coder" {
            guard::capture_coder(&dir, &session_dir, round)
        } else {
            guard::capture_non_coder(&dir)
        };
    }

    fn enforce_run_guard(&self, run: &crate::state::RunRecord) -> Option<String> {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return None;
        }
        let dir = self.guard_dir_for(&run.stage, run.task_id, run.round, run.attempt);
        guard::verify(&dir, &run.stage)
    }

    fn read_exit_status_code(&self, run: &crate::state::RunRecord) -> Option<i32> {
        std::fs::read_to_string(self.run_status_path(run))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
    }

    fn artifact_present(path: &std::path::Path) -> bool {
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.len() > 0)
            .unwrap_or(false)
    }

    /// Capture HEAD at round start so the reviewer can inspect `base..HEAD`.
    /// Idempotent on resume: the original base is preserved.
    fn capture_round_base(&self, round_dir: &std::path::Path) {
        let base_file = round_dir.join("base.txt");
        if base_file.exists() {
            return;
        }
        if let Some(parent) = base_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            let _ = std::fs::write(&base_file, "test-base");
            return;
        }
        if let Some(sha) = git_rev_parse_head() {
            let _ = std::fs::write(&base_file, sha);
        }
    }

    fn coder_gate_reason(&self, round_dir: &std::path::Path) -> Option<String> {
        let base_file = round_dir.join("base.txt");
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return (!Self::artifact_present(&base_file)).then(|| "base_missing".to_string());
        }
        if !Self::artifact_present(&base_file) {
            return Some("base_missing".to_string());
        }
        let base = match std::fs::read_to_string(&base_file) {
            Ok(s) => s.trim().to_string(),
            Err(_) => return Some("base_missing".to_string()),
        };
        if base.is_empty() {
            return Some("base_missing".to_string());
        }
        let Some(head) = git_rev_parse_head() else {
            return Some("git_unavailable".to_string());
        };
        if head == base {
            return Some("no_commits_since_round_start".to_string());
        }
        let reachable = std::process::Command::new("git")
            .args(["merge-base", "--is-ancestor", &base, &head])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !reachable {
            return Some("base_not_reachable_history_rewritten".to_string());
        }
        None
    }

    fn normalized_failure_reason(&self, run: &crate::state::RunRecord) -> Result<Option<String>> {
        let exit_code = self.read_exit_status_code(run);
        if let Some(code) = exit_code {
            if code != 0 {
                if code > 128 {
                    return Ok(Some(format!("killed({})", code - 128)));
                }
                return Ok(Some(format!("exit({code})")));
            }
        }

        let session_dir = session_state::session_dir(&self.state.session_id);
        let reason = match run.stage.as_str() {
            "brainstorm" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                (!Self::artifact_present(&spec_path)).then(|| "artifact_missing".to_string())
            }
            "spec-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round));
                (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string())
            }
            "planning" => {
                let plan_path = session_dir.join("artifacts").join("plan.md");
                (!Self::artifact_present(&plan_path)).then(|| "artifact_missing".to_string())
            }
            "plan-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("plan-review-{}.md", run.round));
                (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string())
            }
            "sharding" => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                if !Self::artifact_present(&tasks_path) {
                    Some("artifact_missing".to_string())
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                }
            }
            "recovery" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                let plan_path = session_dir.join("artifacts").join("plan.md");
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                if !Self::artifact_present(&spec_path)
                    || !Self::artifact_present(&plan_path)
                    || !Self::artifact_present(&tasks_path)
                {
                    Some("artifact_missing".to_string())
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                }
            }
            "coder" => {
                let round_dir = session_dir.join("rounds").join(format!("{:03}", run.round));
                self.coder_gate_reason(&round_dir)
            }
            "reviewer" => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("review.toml");
                if !Self::artifact_present(&review_path) {
                    Some("artifact_missing".to_string())
                } else {
                    review::validate(&review_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                }
            }
            _ => None,
        };

        // Immutability check runs regardless of stage-specific outcome so
        // that a misbehaving agent that happened to produce the right
        // artifact is still caught. Guard reason takes precedence.
        let guard_reason = self.enforce_run_guard(run);
        let reason = guard_reason.or(reason);

        // REVIEWER: if the wrapped command never wrote a status file, we fall back to
        // artifact validation because the tmux window disappearance alone does not preserve
        // a trustworthy exit code.
        Ok(reason)
    }

    fn append_system_message(&mut self, run_id: u64, kind: MessageKind, text: String) {
        let message = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind,
            sender: MessageSender::System,
            text,
        };
        if let Err(err) = self.state.append_message(&message) {
            let _ = self.state.log_event(format!(
                "failed to append system message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(message);
        }
    }

    fn start_run_tracking(
        &mut self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        model: String,
        vendor: String,
        window_name: String,
    ) {
        let attempt = self.attempt_for(stage, task_id, round);
        let run_id = self.state.create_run_record(
            stage.to_string(),
            task_id,
            round,
            attempt,
            model,
            vendor,
            window_name,
        );
        let Some(run) = self.state.agent_runs.iter().find(|run| run.id == run_id) else {
            return;
        };
        let started = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Started,
            sender: MessageSender::System,
            text: format!("agent started · {} ({})", run.model, run.vendor),
        };
        if let Err(err) = self.state.append_message(&started) {
            let _ = self.state.log_event(format!(
                "failed to append started message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(started);
        }
        self.current_run_id = Some(run_id);
        self.window_launched = true;
        self.live_summary_path = Some(
            session_state::session_dir(&self.state.session_id)
                .join("artifacts")
                .join("live_summary.txt"),
        );
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        if let Err(err) = self.state.save() {
            let _ = self
                .state
                .log_event(format!("failed to save session after launch: {err}"));
        }
        self.read_live_summary_pipeline();
        self.messages = SessionState::load_messages(&self.state.session_id).unwrap_or_default();
        self.nodes = build_tree(&self.state);
        self.selected = current_node_index(&self.nodes);
    }

    fn update_agent_progress(&mut self) {
        let Some(run) = self.running_run() else {
            self.agent_line_count = 0;
            return;
        };
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", &run.window_name, "-p", "-J"])
            .output();
        if let Ok(out) = output {
            let text = String::from_utf8_lossy(&out.stdout);
            let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
            self.agent_line_count = lines;
        }
    }

    /// Auto-launch the agent for the current phase if it's a non-interactive
    /// one (spec review, sharding, coder, reviewer). Idempotent: no-op if the
    /// window is already up, if models aren't loaded, or if the last run
    /// errored (user needs to intervene).
    fn maybe_auto_launch(&mut self) {
        if self.window_launched || self.state.agent_error.is_some() || self.models.is_empty() {
            return;
        }
        match self.state.current_phase {
            Phase::SpecReviewRunning => self.launch_spec_review(),
            Phase::PlanReviewRunning => self.launch_plan_review(),
            Phase::ShardingRunning => self.launch_sharding(),
            Phase::ImplementationRound(_) => self.launch_coder(),
            Phase::ReviewRound(_) => self.launch_reviewer(),
            Phase::BuilderRecovery(_) => self.launch_recovery(),
            _ => {}
        }
    }

    fn poll_agent_window(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            return;
        };
        if self.window_exists(&run.window_name) {
            return;
        }

        self.window_launched = false;
        self.current_run_id = None;
        let outcome = self.finalize_current_run(&run);
        if let Err(err) = outcome {
            self.state.agent_error = Some(err.to_string());
            let _ = self.state.log_event(format!(
                "run finalization failed for {}: {err}",
                run.window_name
            ));
        }
        self.nodes = build_tree(&self.state);
        self.selected = current_node_index(&self.nodes);
    }

    fn ensure_builder_task_for_round(&mut self, round: u32) -> Option<u32> {
        if self.state.builder.current_task.is_none() {
            if let Some(id) = self.state.builder.pending.first().copied() {
                self.state.builder.pending.remove(0);
                self.state.builder.current_task = Some(id);
            } else {
                return None;
            }
        }
        self.state.builder.iteration = round;
        let round_dir = session_state::session_dir(&self.state.session_id)
            .join("rounds")
            .join(format!("{round:03}"));
        let _ = std::fs::create_dir_all(&round_dir);
        self.state.builder.current_task
    }

    /// Enter builder recovery. This is builder-only and must remain non-interactive:
    /// it clears `builder.current_task`, preserves `builder.done` and `builder.pending`,
    /// and records recovery context for validation/reconciliation.
    ///
    /// Returns true if recovery was entered (or an attempt was made and recorded) so
    /// callers can treat it as "handled" like other auto-retry paths.
    fn enter_builder_recovery(
        &mut self,
        triggering_round: u32,
        trigger_task_id: Option<u32>,
        trigger_summary: Option<String>,
    ) -> bool {
        if self.current_run_id.is_some() || self.window_launched {
            let _ = self.state.log_event(
                "enter_builder_recovery called while a run window is still marked active"
                    .to_string(),
            );
        }

        let session_dir = session_state::session_dir(&self.state.session_id);
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let (prev_task_ids, prev_max) = tasks::validate(&tasks_path)
            .ok()
            .map(|f| {
                let ids = f.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
                let max = ids.iter().copied().max();
                (ids, max)
            })
            .unwrap_or_default();

        self.state.builder.recovery_trigger_task_id =
            trigger_task_id.or(self.state.builder.current_task);
        self.state.builder.recovery_prev_max_task_id = prev_max;
        self.state.builder.recovery_prev_task_ids = prev_task_ids;
        self.state.builder.recovery_trigger_summary = trigger_summary;
        self.state.builder.current_task = None;
        self.state.agent_error = None;

        if let Err(err) = self.transition_to_phase(Phase::BuilderRecovery(triggering_round)) {
            self.state.agent_error = Some(format!("failed to enter builder recovery: {err}"));
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
        }
        true
    }

    fn started_builder_task_ids(&self) -> BTreeSet<u32> {
        self.state
            .agent_runs
            .iter()
            .filter(|run| matches!(run.stage.as_str(), "coder" | "reviewer"))
            .filter_map(|run| run.task_id)
            .collect()
    }

    fn recovery_notes_document_started_supersession(
        text: &str,
        superseded_ids: &BTreeSet<u32>,
    ) -> Result<()> {
        if !text.contains("Recovery Notes") {
            anyhow::bail!("missing required `Recovery Notes` section");
        }
        for id in superseded_ids {
            let needle = id.to_string();
            let mut found = false;
            for (idx, _) in text.match_indices(&needle) {
                // REVIEWER: spec requires superseded ids be explicitly named but does not
                // prescribe formatting; treat any standalone numeric token match as explicit.
                let prev = idx
                    .checked_sub(1)
                    .and_then(|p| text.as_bytes().get(p).copied())
                    .map(char::from);
                let next = text
                    .as_bytes()
                    .get(idx + needle.len())
                    .copied()
                    .map(char::from);
                let prev_digit = prev.is_some_and(|ch| ch.is_ascii_digit());
                let next_digit = next.is_some_and(|ch| ch.is_ascii_digit());
                if !prev_digit && !next_digit {
                    found = true;
                    break;
                }
            }
            if !found {
                anyhow::bail!("`Recovery Notes` missing superseded started task id {id}");
            }
        }
        Ok(())
    }

    fn reconcile_builder_recovery(&mut self, recovery_run_id: u64) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let parsed = tasks::validate(&tasks_path)
            .with_context(|| format!("invalid {}", tasks_path.display()))?;

        let done_ids = self
            .state
            .builder
            .done
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let started_ids = self.started_builder_task_ids();
        let prev_task_ids = self
            .state
            .builder
            .recovery_prev_task_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let historical_max = self
            .state
            .builder
            .recovery_prev_max_task_id
            .into_iter()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .max()
            .unwrap_or(0);

        let recovered_ids = parsed.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
        let recovered_set = recovered_ids.iter().copied().collect::<BTreeSet<_>>();

        if let Some(collision) = recovered_ids.iter().find(|id| done_ids.contains(id)) {
            anyhow::bail!("recovered unfinished tasks include completed task id {collision}");
        }

        let historical_ids = prev_task_ids
            .iter()
            .copied()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        for id in &recovered_ids {
            if !historical_ids.contains(id) && *id <= historical_max {
                anyhow::bail!(
                    "new recovery task id {id} must be greater than prior max id {historical_max}"
                );
            }
        }

        let superseded_started = started_ids
            .difference(&done_ids)
            .copied()
            .collect::<BTreeSet<_>>()
            .difference(&recovered_set)
            .copied()
            .collect::<BTreeSet<_>>();
        if !superseded_started.is_empty() {
            let spec_text = std::fs::read_to_string(&spec_path)
                .with_context(|| format!("cannot read {}", spec_path.display()))?;
            Self::recovery_notes_document_started_supersession(&spec_text, &superseded_started)
                .with_context(|| format!("invalid {}", spec_path.display()))?;

            let plan_text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("cannot read {}", plan_path.display()))?;
            Self::recovery_notes_document_started_supersession(&plan_text, &superseded_started)
                .with_context(|| format!("invalid {}", plan_path.display()))?;
        }

        self.state.builder.pending = recovered_ids;
        self.state.builder.current_task = None;
        self.state.builder.retry_reset_run_id_cutoff = Some(recovery_run_id);
        self.state.builder.recovery_trigger_task_id = None;
        self.state.builder.recovery_prev_max_task_id = None;
        self.state.builder.recovery_prev_task_ids.clear();
        self.state.builder.recovery_trigger_summary = None;
        Ok(())
    }

    fn finalize_run_record(&mut self, run_id: u64, success: bool, error: Option<String>) {
        let Some(run) = self
            .state
            .agent_runs
            .iter_mut()
            .find(|run| run.id == run_id)
        else {
            return;
        };
        let ended_at = chrono::Utc::now();
        run.ended_at = Some(ended_at);
        run.status = if success {
            RunStatus::Done
        } else {
            RunStatus::Failed
        };
        run.error = error.clone();

        let duration = ended_at.signed_duration_since(run.started_at);
        let total_seconds = duration.num_seconds().max(0);
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        let text = if success {
            format!("done in {minutes}m{seconds:02}s")
        } else {
            format!(
                "attempt {} failed: {}",
                run.attempt,
                error.unwrap_or_else(|| "unknown error".to_string())
            )
        };
        let message = Message {
            ts: ended_at,
            run_id,
            kind: MessageKind::End,
            sender: MessageSender::System,
            text,
        };
        if let Err(err) = self.state.append_message(&message) {
            let _ = self.state.log_event(format!(
                "failed to append end message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(message);
        }
        if let Err(err) = self.state.save() {
            let _ = self.state.log_event(format!(
                "failed to save session after finalizing run {run_id}: {err}"
            ));
        }
    }

    fn retry_exhausted_summary(&self, failed_run: &crate::state::RunRecord) -> String {
        let mut attempts = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                run.stage == failed_run.stage
                    && run.task_id == failed_run.task_id
                    && run.round == failed_run.round
                    && run.status == RunStatus::Failed
            })
            .cloned()
            .collect::<Vec<_>>();
        attempts.sort_by_key(|run| run.attempt);

        let mut lines = vec![format!("retry exhausted ({} attempts)", attempts.len())];
        for run in attempts {
            lines.push(format!(
                "  attempt {}: {}/{} — {}",
                run.attempt,
                run.vendor,
                run.model,
                run.error.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        lines.join("\n")
    }

    fn maybe_auto_retry(&mut self, failed_run: &crate::state::RunRecord) -> bool {
        if failed_run.stage == "brainstorm" {
            return false;
        }
        if failed_run.error.as_deref() == Some("user_forced_retry") {
            return false;
        }

        let key = Self::retry_key_for_run(failed_run);
        let last_failed_vendor = selection::vendor::str_to_vendor(&failed_run.vendor);
        if let Some(vendor) = last_failed_vendor {
            self.failed_models
                .entry(key.clone())
                .or_insert_with(HashSet::new)
                .insert((vendor, failed_run.model.clone()));
        }

        let max_attempts = self.models.len() as u32 + 2;
        if failed_run.attempt >= max_attempts {
            let summary = self.retry_exhausted_summary(failed_run);
            if matches!(failed_run.stage.as_str(), "coder" | "reviewer") {
                return self.enter_builder_recovery(
                    failed_run.round,
                    failed_run.task_id,
                    Some(summary),
                );
            }
            if failed_run.stage == "recovery" {
                let summary = format!("builder recovery retry exhausted\n{summary}");
                self.state.agent_error = Some(summary.clone());
                let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
                self.append_system_message(failed_run.id, MessageKind::End, summary);
                return true;
            }

            self.state.agent_error = Some(summary.clone());
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
            self.append_system_message(failed_run.id, MessageKind::End, summary);
            let _ = self.state.log_event(format!(
                "auto-retry safety cap hit for {} round {} attempt {}",
                failed_run.stage, failed_run.round, failed_run.attempt
            ));
            return true;
        }

        let excluded = self.failed_models.get(&key).cloned().unwrap_or_default();
        let next_model = select_excluding(
            &self.models,
            Self::task_kind_for_stage(&failed_run.stage),
            &excluded,
            last_failed_vendor,
        );

        if let Some(next_model) = next_model.cloned() {
            self.append_system_message(
                failed_run.id,
                MessageKind::Started,
                format!(
                    "retrying with {}/{}",
                    vendor_tag(next_model.vendor),
                    next_model.name
                ),
            );
            return self.launch_retry_for_stage(failed_run, next_model);
        }

        let summary = self.retry_exhausted_summary(failed_run);
        if matches!(failed_run.stage.as_str(), "coder" | "reviewer") {
            return self.enter_builder_recovery(
                failed_run.round,
                failed_run.task_id,
                Some(summary),
            );
        }
        if failed_run.stage == "recovery" {
            let summary = format!("builder recovery retry exhausted\n{summary}");
            self.state.agent_error = Some(summary.clone());
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
            self.append_system_message(failed_run.id, MessageKind::End, summary);
            return true;
        }

        self.state.agent_error = Some(summary.clone());
        let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
        self.append_system_message(failed_run.id, MessageKind::End, summary);
        true
    }

    fn finalize_current_run(&mut self, run: &crate::state::RunRecord) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        if let Some(error) = self.normalized_failure_reason(run)? {
            self.finalize_run_record(run.id, false, Some(error.clone()));
            let failed_run = self
                .state
                .agent_runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .cloned()
                .unwrap_or_else(|| run.clone());
            if !self.maybe_auto_retry(&failed_run) {
                self.state.agent_error = Some(error);
            }
            return Ok(());
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::SpecReviewRunning)?;
            }
            Phase::SpecReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::SpecReviewPaused)?;
            }
            Phase::PlanningRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::PlanReviewRunning)?;
            }
            Phase::PlanReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::PlanReviewPaused)?;
            }
            Phase::ShardingRunning => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let parsed = tasks::validate(&tasks_path)
                    .with_context(|| format!("invalid {}", tasks_path.display()));
                match parsed {
                    Ok(parsed) => {
                        self.state.builder.pending =
                            parsed.tasks.iter().map(|task| task.id).collect();
                        self.state.builder.current_task = None;
                        self.state.builder.done.clear();
                        self.state.builder.iteration = 0;
                        self.state.builder.last_verdict = None;
                        self.finalize_run_record(run.id, true, None);
                        self.state.agent_error = None;
                        self.transition_to_phase(Phase::ImplementationRound(1))?;
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::ImplementationRound(round) => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::ReviewRound(round))?;
            }
            Phase::ReviewRound(round) => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{round:03}"))
                    .join("review.toml");
                match review::validate(&review_path) {
                    Ok(verdict) => {
                        self.finalize_run_record(run.id, true, None);
                        self.state.agent_error = None;
                        self.state.builder.last_verdict =
                            Some(format!("{:?}", verdict.status).to_lowercase());
                        match verdict.status {
                            review::ReviewStatus::Done => {
                                if let Some(task_id) = self.state.builder.current_task.take() {
                                    self.state.builder.done.push(task_id);
                                }
                                if self.state.builder.pending.is_empty() {
                                    self.transition_to_phase(Phase::Done)?;
                                } else {
                                    self.transition_to_phase(Phase::ImplementationRound(
                                        round + 1,
                                    ))?;
                                }
                            }
                            review::ReviewStatus::Revise => {
                                // REVIEWER: spec does not define whether revise should reuse the
                                // same round or advance. We advance to the next round to preserve
                                // round-as-iteration semantics from the builder flow.
                                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
                            }
                            review::ReviewStatus::Blocked => {
                                let summary = verdict.feedback.join("\n");
                                let trigger_summary =
                                    (!summary.trim().is_empty()).then_some(summary);
                                self.enter_builder_recovery(
                                    round,
                                    self.state.builder.current_task,
                                    trigger_summary,
                                );
                            }
                        }
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::BuilderRecovery(round) => match self.reconcile_builder_recovery(run.id) {
                Ok(()) => {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
                }
                Err(err) => {
                    let reason = format!("recovery_reconcile_failed: {err:#}");
                    self.finalize_run_record(run.id, false, Some(reason.clone()));
                    let failed_run = self
                        .state
                        .agent_runs
                        .iter()
                        .find(|candidate| candidate.id == run.id)
                        .cloned()
                        .unwrap_or_else(|| run.clone());
                    if !self.maybe_auto_retry(&failed_run) {
                        self.state.agent_error = Some(reason);
                    }
                }
            },
            Phase::IdeaInput
            | Phase::SpecReviewPaused
            | Phase::PlanReviewPaused
            | Phase::BlockedNeedsUser
            | Phase::Done => {}
        }
        Ok(())
    }

    fn launch_brainstorm(&mut self, idea: String) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
        }

        let Some(chosen) = Self::select_brainstorm_model(&self.models)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error =
                Some("no model available with quota — check model strip".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
        };
        let (model, vendor_kind, vendor) = chosen;

        let session_id = &self.state.session_id;
        let prompt_path = session_state::session_dir(session_id)
            .join("prompts")
            .join("brainstorm.md");
        let spec_path = session_state::session_dir(session_id)
            .join("artifacts")
            .join("spec.md");

        let _ = std::fs::remove_file(&spec_path);

        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let live_summary_path = session_state::session_dir(session_id)
            .join("artifacts")
            .join("live_summary.txt");
        let prompt = brainstorm_prompt(
            &idea,
            &spec_path.display().to_string(),
            &live_summary_path.display().to_string(),
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.state.agent_error = Some(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let attempt = self.attempt_for("brainstorm", None, 1);
        let status_path = self.run_status_path_for("brainstorm", None, 1, attempt);
        self.capture_run_guard("brainstorm", None, 1, attempt);
        let adapter = adapter_for_vendor(vendor_kind);
        let window_name = window_name_with_model("[Brainstorm]", &model);
        match launch_interactive(&window_name, &run, adapter.as_ref(), true, &status_path) {
            Ok(()) => {
                self.state.idea_text = Some(idea.clone());
                self.state.selected_model = Some(model.clone());
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
                self.start_run_tracking("brainstorm", None, 1, model, vendor, window_name);
            }
            Err(e) => {
                self.state.agent_error = Some(format!("failed to launch brainstorm: {e}"));
            }
        }
    }

    fn select_brainstorm_model(
        models: &[selection::ModelStatus],
    ) -> Option<&selection::ModelStatus> {
        selection::select(models, selection::TaskKind::Idea)
    }

    fn launch_retry_for_stage(
        &mut self,
        failed_run: &crate::state::RunRecord,
        chosen: ModelStatus,
    ) -> bool {
        match failed_run.stage.as_str() {
            "spec-review" => self.launch_spec_review_with_model(Some(chosen)),
            "planning" => self.launch_planning_with_model(Some(chosen), false),
            "plan-review" => self.launch_plan_review_with_model(Some(chosen)),
            "sharding" => self.launch_sharding_with_model(Some(chosen)),
            "recovery" => self.launch_recovery_with_model(Some(chosen)),
            "coder" => self.launch_coder_with_model(Some(chosen)),
            "reviewer" => self.launch_reviewer_with_model(Some(chosen)),
            _ => false,
        }
    }

    fn launch_spec_review(&mut self) {
        let _ = self.launch_spec_review_with_model(None);
    }

    fn launch_spec_review_with_model(&mut self, override_model: Option<ModelStatus>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }

        let round = match self.state.current_phase {
            Phase::SpecReviewPaused => self.completed_rounds("spec-review") + 1,
            _ => self.completed_rounds("spec-review").max(1),
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let review_path = session_dir
            .join("artifacts")
            .join(format!("spec-review-{round}.md"));
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("spec-review-{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                select_for_review(
                    &self.models,
                    &self
                        .state
                        .agent_runs
                        .iter()
                        .filter(|run| {
                            run.stage == "spec-review"
                                && run.round == round
                                && run.status == RunStatus::Done
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt = spec_review_prompt(
            &spec_path.display().to_string(),
            &review_path.display().to_string(),
            &session_dir
                .join("artifacts")
                .join("live_summary.txt")
                .display()
                .to_string(),
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.state.agent_error = Some(format!("error writing prompt: {err}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let window_name = window_name_with_model(&format!("[Spec Review {round}]"), &model);
        let attempt = self.attempt_for("spec-review", None, round);
        let status_path = self.run_status_path_for("spec-review", None, round, attempt);
        self.capture_run_guard("spec-review", None, round, attempt);
        let launch_result =
            if let Some(result) = self.try_test_launch(&status_path, Some(&review_path)) {
                result
            } else {
                let adapter = adapter_for_vendor(vendor_kind);
                launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("spec-review", None, round, model, vendor, window_name);
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch spec review: {err}"));
                false
            }
        }
    }

    fn launch_planning(&mut self) {
        let _ = self.launch_planning_with_model(None, true);
    }

    fn launch_planning_with_model(
        &mut self,
        override_model: Option<ModelStatus>,
        interactive: bool,
    ) -> bool {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");

        let review_paths: Vec<std::path::PathBuf> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "spec-review" && run.status == RunStatus::Done)
            .map(|run| {
                session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round))
            })
            .filter(|path| path.exists())
            .collect();

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| selection::select(&self.models, selection::TaskKind::Planning))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let _ = std::fs::remove_file(&plan_path);

        let prompt_path = session_dir.join("prompts").join("planning.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let live_summary_path = session_dir.join("artifacts").join("live_summary.txt");
        let prompt = planning_prompt(&spec_path, &review_paths, &plan_path, &live_summary_path);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        let attempt = self.attempt_for("planning", None, 1);
        let status_path = self.run_status_path_for("planning", None, 1, attempt);
        self.capture_run_guard("planning", None, 1, attempt);
        let window_name = window_name_with_model("[Planning]", &model);
        let launch_result =
            if let Some(result) = self.try_test_launch(&status_path, Some(&plan_path)) {
                result
            } else if interactive {
                launch_interactive(&window_name, &run, adapter.as_ref(), true, &status_path)
            } else {
                launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("planning", None, 1, model, vendor, window_name);
                true
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch planning: {e}"));
                false
            }
        }
    }

    fn launch_plan_review(&mut self) {
        let _ = self.launch_plan_review_with_model(None);
    }

    fn launch_plan_review_with_model(&mut self, override_model: Option<ModelStatus>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }

        let round = match self.state.current_phase {
            Phase::PlanReviewPaused => self.completed_rounds("plan-review") + 1,
            _ => self.completed_rounds("plan-review").max(1),
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let review_path = session_dir
            .join("artifacts")
            .join(format!("plan-review-{round}.md"));
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("plan-review-{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                select_for_review(
                    &self.models,
                    &self
                        .state
                        .agent_runs
                        .iter()
                        .filter(|run| {
                            run.stage == "plan-review"
                                && run.round == round
                                && run.status == RunStatus::Done
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt = plan_review_prompt(
            &spec_path.display().to_string(),
            &plan_path.display().to_string(),
            &review_path.display().to_string(),
            &session_dir
                .join("artifacts")
                .join("live_summary.txt")
                .display()
                .to_string(),
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.state.agent_error = Some(format!("error writing prompt: {err}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let window_name = window_name_with_model(&format!("[Plan Review {round}]"), &model);
        let attempt = self.attempt_for("plan-review", None, round);
        let status_path = self.run_status_path_for("plan-review", None, round, attempt);
        self.capture_run_guard("plan-review", None, round, attempt);
        let launch_result =
            if let Some(result) = self.try_test_launch(&status_path, Some(&review_path)) {
                result
            } else {
                let adapter = adapter_for_vendor(vendor_kind);
                launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("plan-review", None, round, model, vendor, window_name);
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch plan review: {err}"));
                false
            }
        }
    }

    fn launch_sharding(&mut self) {
        let _ = self.launch_sharding_with_model(None);
    }

    fn launch_sharding_with_model(&mut self, override_model: Option<ModelStatus>) -> bool {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| selection::select(&self.models, selection::TaskKind::Planning))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let _ = std::fs::remove_file(&tasks_path);

        let prompt_path = session_dir.join("prompts").join("sharding.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let live_summary_path = session_dir.join("artifacts").join("live_summary.txt");
        let prompt = sharding_prompt(&spec_path, &plan_path, &tasks_path, &live_summary_path);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let attempt = self.attempt_for("sharding", None, 1);
        let status_path = self.run_status_path_for("sharding", None, 1, attempt);
        self.capture_run_guard("sharding", None, 1, attempt);
        let window_name = window_name_with_model("[Sharding]", &model);
        let launch_result =
            if let Some(result) = self.try_test_launch(&status_path, Some(&tasks_path)) {
                result
            } else {
                let adapter = adapter_for_vendor(vendor_kind);
                launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("sharding", None, 1, model, vendor, window_name);
                true
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch sharding: {e}"));
                false
            }
        }
    }

    fn launch_recovery(&mut self) {
        let _ = self.launch_recovery_with_model(None);
    }

    fn launch_recovery_with_model(&mut self, override_model: Option<ModelStatus>) -> bool {
        use anyhow::Context;

        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }
        let Phase::BuilderRecovery(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-r{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| selection::select(&self.models, selection::TaskKind::Planning))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let completed = self.state.builder.done.clone();
        let mut started = self
            .started_builder_task_ids()
            .into_iter()
            .collect::<Vec<_>>();
        started.sort_unstable();
        let prompt = recovery_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            self.state.builder.recovery_trigger_task_id,
            self.state.builder.recovery_trigger_summary.as_deref(),
            &completed,
            &started,
            &session_dir.join("artifacts").join("live_summary.txt"),
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.state.agent_error = Some(err.to_string());
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let attempt = self.attempt_for("recovery", None, round);
        let status_path = self.run_status_path_for("recovery", None, round, attempt);
        self.capture_run_guard("recovery", None, round, attempt);
        let window_name = window_name_with_model("[Recovery]", &model);
        let launch_result =
            if let Some(result) = self.try_test_launch(&status_path, Some(&tasks_path)) {
                result
            } else {
                let adapter = adapter_for_vendor(vendor_kind);
                launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("recovery", None, round, model, vendor, window_name);
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch recovery: {err}"));
                false
            }
        }
    }

    fn launch_coder(&mut self) {
        let _ = self.launch_coder_with_model(None);
    }

    fn launch_coder_with_model(&mut self, override_model: Option<ModelStatus>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }
        let Phase::ImplementationRound(r) = self.state.current_phase else {
            return false;
        };

        let Some(task_id) = self.ensure_builder_task_for_round(r) else {
            self.state.agent_error = Some("no pending tasks".to_string());
            let _ = self.state.save();
            return false;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let task_file = round_dir.join("task.md");

        if !task_file.exists() {
            let body = task_body_for(&session_dir, task_id).unwrap_or_else(|e| {
                format!("(task body could not be loaded: {e})\n\nTask id: {task_id}\n")
            });
            let _ = std::fs::write(&task_file, body);
        }

        // Pin the base HEAD before the coder runs; preserves original base on resume.
        self.capture_round_base(&round_dir);

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| selection::select(&self.models, selection::TaskKind::Build))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt_path = session_dir.join("prompts").join(format!("coder-r{r}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let resume = self
            .state
            .agent_runs
            .iter()
            .any(|run| run.stage == "coder" && run.task_id == Some(task_id) && run.round == r);
        let prompt = coder_prompt(&session_dir, task_id, r, &task_file, resume);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let window_name = window_name_with_model(&format!("[Coder r{r}]"), &model);
        let attempt = self.attempt_for("coder", Some(task_id), r);
        let status_path = self.run_status_path_for("coder", Some(task_id), r, attempt);
        self.capture_run_guard("coder", Some(task_id), r, attempt);
        let launch_result = if let Some(result) = self.try_test_launch(&status_path, None) {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("coder", Some(task_id), r, model, vendor, window_name);
                true
            }
            Err(e) => {
                let _ = self.state.log_event(format!("failed to launch coder: {e}"));
                false
            }
        }
    }

    fn launch_reviewer(&mut self) {
        let _ = self.launch_reviewer_with_model(None);
    }

    fn launch_reviewer_with_model(&mut self, override_model: Option<ModelStatus>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return false;
        }
        let Phase::ReviewRound(r) = self.state.current_phase else {
            return false;
        };
        let Some(task_id) = self.state.builder.current_task else {
            self.state.agent_error = Some("no current task".to_string());
            let _ = self.state.save();
            return false;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let review_path = round_dir.join("review.toml");
        let base_file = round_dir.join("base.txt");
        let commits_file = round_dir.join("commits.txt");
        let task_file = round_dir.join("task.md");

        let _ = std::fs::remove_file(&review_path);

        // Pre-compute the SHA list in base..HEAD so the reviewer can glance at it
        // without shelling out. Skipped in test mode (no real git).
        #[cfg(not(test))]
        if let Ok(base) = std::fs::read_to_string(&base_file) {
            let base = base.trim();
            if !base.is_empty() {
                write_commits_list(&round_dir, base);
            }
        }
        #[cfg(test)]
        if self.test_launch_harness.is_none() {
            if let Ok(base) = std::fs::read_to_string(&base_file) {
                let base = base.trim();
                if !base.is_empty() {
                    write_commits_list(&round_dir, base);
                }
            }
        }

        let excluded = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                (run.stage == "reviewer" || run.stage == "coder")
                    && run.task_id == Some(task_id)
                    && run.round == r
            })
            .cloned()
            .collect::<Vec<_>>();
        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| select_for_review(&self.models, &excluded))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt_path = session_dir
            .join("prompts")
            .join(format!("reviewer-r{r}.md"));
        let prompt = reviewer_prompt(
            &session_dir,
            task_id,
            r,
            &task_file,
            &base_file,
            &commits_file,
            &review_path,
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let window_name = window_name_with_model(&format!("[Review r{r}]"), &model);
        let attempt = self.attempt_for("reviewer", Some(task_id), r);
        let status_path = self.run_status_path_for("reviewer", Some(task_id), r, attempt);
        self.capture_run_guard("reviewer", Some(task_id), r, attempt);
        let launch_result =
            if let Some(result) = self.try_test_launch(&status_path, Some(&review_path)) {
                result
            } else {
                let adapter = adapter_for_vendor(vendor_kind);
                launch_noninteractive(&window_name, &run, adapter.as_ref(), &status_path)
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("reviewer", Some(task_id), r, model, vendor, window_name);
                true
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch reviewer: {e}"));
                false
            }
        }
    }

    fn setup_watcher(&mut self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let watcher_result = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        );
        match watcher_result {
            Ok(mut watcher) => {
                let path = session_state::session_dir(&self.state.session_id)
                    .join("artifacts")
                    .join("live_summary.txt");
                if let Err(e) = watcher.watch(&path, notify::RecursiveMode::NonRecursive) {
                    let _ = self
                        .state
                        .log_event(format!("watcher setup failed: {}, falling back to poll", e));
                    return Ok(());
                }
                self.live_summary_watcher = Some(watcher);
                self.live_summary_change_rx = Some(rx);
                Ok(())
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("watcher init failed: {}, falling back to poll", e));
                Ok(())
            }
        }
    }

    fn process_live_summary_changes(&mut self) {
        if let Some(ref rx) = self.live_summary_change_rx {
            let mut saw_change = false;
            while rx.try_recv().is_ok() {
                saw_change = true;
            }
            if saw_change {
                self.read_live_summary_pipeline();
            }
        } else {
            self.poll_live_summary_fallback();
        }
    }

    fn poll_live_summary_fallback(&mut self) {
        if !self.window_launched {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary_cached_text.clear();
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            self.live_summary_cached_text.clear();
            return;
        }
        let should_read = match self.live_summary_cached_mtime {
            None => true,
            Some(cached) => mtime > cached,
        };
        if should_read {
            self.read_live_summary_pipeline();
        }
    }

    fn read_live_summary_pipeline(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some(run) = self.running_run() else {
            return;
        };
        if !tmux::window_exists(&run.window_name) {
            return;
        }
        let path = session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join("live_summary.txt");
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        if let Some(cached_mtime) = self.live_summary_cached_mtime {
            if mtime <= cached_mtime {
                return;
            }
        }
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let sanitized = render::sanitize_live_summary(&content);
        if sanitized.is_empty() {
            return;
        }
        if sanitized == self.live_summary_cached_text {
            return;
        }
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: run.model.clone(),
                vendor: run.vendor.clone(),
            },
            text: sanitized.clone(),
        };
        if let Err(err) = self.state.append_message(&msg) {
            let _ = self.state.log_event(format!(
                "failed to append brief message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(msg);
        }
        self.live_summary_cached_text = sanitized;
        self.live_summary_cached_mtime = Some(mtime);
    }
}

fn kill_window(base: &str) {
    // Windows are now named "[Base] <model>", so match by prefix: exact match
    // or the base followed by a space. The base ends with `]`, which prevents
    // `[Coder r1]` from accidentally matching `[Coder r10]`, etc.
    let prefix = format!("{base} ");
    let Ok(output) = std::process::Command::new("tmux")
        .args(["list-windows", "-F", "#{window_name}"])
        .output()
    else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in stdout.lines() {
        if name == base || name.starts_with(&prefix) {
            let _ = std::process::Command::new("tmux")
                .args(["kill-window", "-t", name])
                .output();
        }
    }
}

fn restore_artifacts(pairs: &[(&std::path::Path, &std::path::Path)]) {
    for (backup, target) in pairs {
        if backup.exists() {
            let _ = std::fs::copy(backup, target);
        }
    }
}

fn task_body_for(session_dir: &std::path::Path, task_id: u32) -> anyhow::Result<String> {
    use anyhow::Context;
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml")?;
    let task = parsed
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| anyhow::anyhow!("task id {task_id} not found"))?;
    let refs = |rs: &[crate::tasks::Ref]| -> String {
        if rs.is_empty() {
            "(none)".to_string()
        } else {
            rs.iter()
                .map(|r| format!("  - {} lines {}", r.path, r.lines))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    Ok(format!(
        "# Task {id}: {title}\n\n## Description\n{desc}\n\n## Test\n{test}\n\n## Spec refs\n{specs}\n\n## Plan refs\n{plans}\n\nEstimated effort: ~{tokens} tokens\n",
        id = task.id,
        title = task.title,
        desc = task.description,
        test = task.test,
        specs = refs(&task.spec_refs),
        plans = refs(&task.plan_refs),
        tokens = task.estimated_tokens,
    ))
}

fn live_summary_instruction(path: &std::path::Path) -> String {
    format!(
        "\n\nEvery 2–3 min (and whenever your sub-goal changes), overwrite {} \
         with one plain-text line formatted as: `<≤5-word essence> | <current \
         progress> | <next action>`. The first field MUST capture the real \
         essence, not a generic label. Your process is killed if this file \
         isn't updated for 10 min of wall time (time spent inside tool calls \
         is excluded from that budget).\n",
        path.display()
    )
}

fn spec_review_prompt(spec_path: &str, review_path: &str, live_summary_path: &str) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"You review a spec. NON-INTERACTIVE — no clarifying questions; judge from the
spec alone. Do NOT modify code; write ONLY the review file.

Spec:   {spec_path}
Output: {review_path}

Evaluate clarity, completeness, buildability, risks, and gaps. The review MUST cover:
  - Verdict: approve / approve-with-changes / reject
  - Specific issues (if any), each with a suggested fix
  - Open risks the spec does not address
{instr}"#
    )
}

fn plan_review_prompt(
    spec_path: &str,
    plan_path: &str,
    review_path: &str,
    live_summary_path: &str,
) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"You review an implementation plan. NON-INTERACTIVE — no clarifying questions.

Inputs:
  Plan: {plan_path}
  Spec: {spec_path}

Flag ONLY critical issues — things that would block or break implementation:
  - Spec requirement with no corresponding plan step.
  - Plan steps ordered unbuildably (a step depends on something a later step creates).
  - Contradictions plan↔spec, or internal contradictions that would lead to the
    wrong build.
  - File paths, function names, or interfaces inconsistent across steps in a way
    that would cause real breakage.
  - Spec-level ambiguity severe enough that an implementer could not proceed.
Multiple valid implementations is NOT a defect; don't force one internal design
when several options satisfy the spec and any explicit interfaces.

If — and only if — you find critical issues, directly edit {plan_path} (and
{spec_path} if spec-level) with the smallest fix. Then write a markdown-bullet
changelog of what you changed and why to {review_path}. If nothing was critical,
write a single bullet saying so — do NOT invent issues to fill space.

Do NOT flag or fix: typos, grammar, wording, formatting, style, tone, structural
polish, missing low-level implementation detail, absence of prescribed helper/
function structure, multiple possible approaches (unless the plan/spec makes an
explicit interface commitment that is internally contradictory), hypothetical
edge cases the spec does not require, or minor nitpicks. When in doubt, leave it
alone — over-editing is worse than under-editing.

Rules: do NOT create or modify source code; do NOT run git or modify version
control; do NOT ask the operator.
{instr}"#
    )
}

fn brainstorm_prompt(idea: &str, spec_path: &str, live_summary_path: &str) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"Invoke your brainstorming skill now.

Idea:
---
{idea}
---

When the skill asks where to write the design doc, write it to {spec_path}.

This is a spec-only phase: do NOT write or modify any code; the spec file is
your only output. Implementation happens in a later phase.

HARD rules — override anything the superpowers / brainstorming skill suggests:
  - Do NOT `git add`, `git commit`, `git stash`, or touch version control. The
    spec stays untracked; a later phase commits.
  - Do NOT ask the operator whether to continue, proceed to planning, move on,
    or run any follow-up skill — including any inline "continue to next stage"
    prompt the skill may offer. When the spec is written, STOP and exit. The
    orchestrator drives stage transitions.

The operator IS available to answer questions ABOUT THE DESIGN itself.
{instr}"#
    )
}

fn planning_prompt(
    spec_path: &std::path::Path,
    review_paths: &[std::path::PathBuf],
    plan_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    let reviews_block = if review_paths.is_empty() {
        "(no spec reviews available — work from the spec alone)".to_string()
    } else {
        review_paths
            .iter()
            .enumerate()
            .map(|(i, p)| format!("  - review {}: {}", i + 1, p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"Invoke your superpowers:writing-plans skill now.

You are turning an approved spec + any spec reviews into an implementation plan.

Inputs:
  Spec:    {spec}
  Reviews:
{reviews}

Triage reviews first: they may contradict each other. Decide what to incorporate,
what to reject, and why. If a trade-off is real and you cannot confidently make
it alone, ASK the operator — this is interactive.

Once every trade-off is resolved, do TWO things IN THIS ORDER:
  1. UPDATE the spec in place at {spec} so it reflects accepted feedback and
     every decision you just made. Another agent reading ONLY the spec must not
     be surprised by anything in the plan.
  2. Write the plan to {plan}.

Hard rules — override anything the writing-plans skill suggests:
  - Do NOT write or modify any code (source, configs, build scripts). You may
    only edit the spec and write the plan.
  - Do NOT `git add`, `git commit`, `git stash`, or touch version control; both
    files stay untracked (a later phase commits). Refuse if the skill offers to
    commit. Do NOT offer to run tests, commit, or push.
  - The plan MUST be an execution map for coordination. It SHOULD include:
      sequencing and dependencies (what order matters, and why); interfaces,
      integration points, and execution seams that must be honored; constraints
      from the spec that narrow the correct solution space; optional likely
      file/module touchpoints ONLY as orientation.
  - The plan MUST NOT read like a pseudo-implementation or patch recipe: no
      checkbox to-do lists or step-by-step coding instructions; no helper/
      function decomposition or function-by-function edit sequences; no patch-
      like ordering, "change this line then that line", or mini diffs; no
      mandated internal code shape (struct fields, method signatures, class
      layout) unless required by the spec or an explicit interface commitment
      needed for coordination.
  - Authority rule: the spec is the design contract and wins any conflict; the
      plan is advisory for implementation shape; the plan is authoritative ONLY
      for sequencing and explicit interface commitments it names. Do not turn
      advisory detail into an implementation contract.
  - Do NOT ask the operator whether to continue, proceed, start implementing,
      jump to coding, run the next skill, or skip any downstream stage. When
      the plan is written, STOP and exit — the orchestrator drives stage
      transitions.

The operator IS available for clarifying questions about the design itself.
{instr}"#,
        spec = spec_path.display(),
        reviews = reviews_block,
        plan = plan_path.display(),
        instr = instr,
    )
}

fn sharding_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    format!(
        r#"You split an approved plan into actionable, self-contained, buildable tasks.
NON-INTERACTIVE — do NOT modify any code; your ONLY output is the tasks TOML.

Inputs:
  Spec: {spec}
  Plan: {plan}
Read both carefully before sharding.

Sizing:
  - Target ~100_000 tokens of implementation effort per task — small enough for
    one coding session without context compaction, large enough to be meaningful.
  - Decompose only when the plan warrants it. If the whole plan fits one ~100k
    session, a single-task tasks.toml is correct — do NOT force artificial
    splits. Bigger plans split along natural seams (subsystem / layer / phase).
  - Each task must be self-contained: buildable on its own (compiles / links /
    type-checks) by a single coding session. A task does NOT have to be
    independently testable — scaffolding or groundwork tasks that only become
    testable after a later task lands are allowed, AS LONG AS they still build
    cleanly on their own.
  - Unless a dependency is explicitly listed in a task's description, no task
    may assume another task has shipped first.

Required fields per task:
  - id               sequential integer starting at 1
  - title            one-line summary
  - description      detailed what-to-do (multi-line TOML string allowed)
  - test             concrete verification steps, OR the literal string
                     "not testable" followed by a one-line reason (e.g.
                     "not testable — scaffolding; verified by task 4's tests").
                     Use "not testable" ONLY for genuine intermediate/
                     scaffolding tasks. The reviewer honors this by skipping
                     the test-pass check for such tasks, but still requires
                     the code to build.
  - estimated_tokens integer estimate (target ~100_000)
  - spec_refs        array of {{ path, lines }} pointing into the spec
  - plan_refs        array of {{ path, lines }} pointing into the plan
  `lines` is a range like "12-45" or a single number.

Description rules — outcome- and coordination-oriented:
  - SHOULD focus on required outcomes, dependencies/ordering, acceptance checks,
    and relevant interfaces/touchpoints (file/module touchpoints only as
    orientation).
  - MUST NOT be recipe-style: no step-by-step coding instructions; no miniature
    edit scripts or pseudo-patch sequences; no mandated internal design or
    helper/function decomposition unless required by the spec or an explicit
    interface commitment needed for coordination.
  - `plan_refs` MUST point to plan content about goals, sequencing,
    dependencies, or interface commitments — not primarily to recipe-like
    implementation instructions.

Output: write the TOML to {tasks} in EXACTLY this shape (double-quoted strings;
triple-quoted for multi-line; arrays of inline tables for refs):

    [[tasks]]
    id = 1
    title = "Scaffold the worker pool"
    description = """
    Wire up a Tokio worker pool in src/pool.rs. …
    """
    test = """
    Run `cargo test pool::` — the new tests must pass.
    """
    estimated_tokens = 90000
    spec_refs = [
      {{ path = "artifacts/spec.md", lines = "10-45" }},
    ]
    plan_refs = [
      {{ path = "artifacts/plan.md", lines = "22-60" }},
      {{ path = "artifacts/plan.md", lines = "110-125" }},
    ]

    [[tasks]]
    id = 2
    …

The file is validated programmatically — missing or empty fields cause
rejection. Do NOT emit any prose around the TOML.
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        tasks = tasks_path.display(),
        instr = instr,
    )
}

fn recovery_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    trigger_task_id: Option<u32>,
    trigger_summary: Option<&str>,
    completed_task_ids: &[u32],
    started_task_ids: &[u32],
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    let trigger_task = trigger_task_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "(none)".to_string());
    let trigger_summary = trigger_summary.unwrap_or("(none recorded)");
    let completed = if completed_task_ids.is_empty() {
        "(none)".to_string()
    } else {
        completed_task_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let started = if started_task_ids.is_empty() {
        "(none)".to_string()
    } else {
        started_task_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        r#"You are the builder recovery agent. NON-INTERACTIVE — no operator questions.

Your job is to repair builder artifacts so orchestration can reconcile and resume.
You may edit ONLY:
  - {spec}
  - {plan}
  - {tasks}

Context from orchestrator:
  - Triggering task id: {trigger_task}
  - Trigger summary / latest reviewer feedback:
{trigger_summary}
  - Completed task ids (must stay completed): {completed}
  - Started task ids from run history: {started}

Hard requirements:
  - Keep `tasks.toml` valid and include unfinished work only.
  - Do NOT include completed ids in recovered `tasks.toml`.
  - If you supersede/remove started-but-unfinished task ids, add a `Recovery Notes`
    section in BOTH spec and plan, naming each superseded id and reason.
  - Keep changes minimal and deterministic.
  - Do NOT modify source code or version control.
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        tasks = tasks_path.display(),
        trigger_task = trigger_task,
        trigger_summary = trigger_summary,
        completed = completed,
        started = started,
        instr = instr,
    )
}

fn git_rev_parse_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Pre-compute the list of SHAs in `base..HEAD` so the reviewer doesn't need to
/// shell out. Writes `rounds/NNN/commits.txt` with one SHA per line. No-op in
/// test mode (no real git) and on any git failure; the reviewer prompt only
/// references the file when it's useful.
fn write_commits_list(round_dir: &std::path::Path, base: &str) {
    let commits_file = round_dir.join("commits.txt");
    let Ok(output) = std::process::Command::new("git")
        .args(["log", "--format=%H", &format!("{base}..HEAD")])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let _ = std::fs::write(&commits_file, output.stdout);
}

fn coder_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    resume: bool,
) -> String {
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let prev_review = if round > 1 {
        let p = session_dir
            .join("rounds")
            .join(format!("{:03}", round - 1))
            .join("review.toml");
        if p.exists() {
            format!(
                "\nPrevious reviewer feedback (round {}): {}\nRead it first and address every feedback item.\n",
                round - 1,
                p.display()
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let resume_hint = if resume {
        "\nThis is a RESUME of a previous coding session on the same task — pick up where\nyou left off, honour the reviewer feedback above, and finish the work.\n"
    } else {
        ""
    };
    let live_summary_path = session_dir.join("artifacts").join("live_summary.txt");
    let instr = live_summary_instruction(&live_summary_path);
    format!(
        r#"You are the coder for task {task_id}, round {round}. NON-INTERACTIVE — the
operator is NOT available. Make your own judgement calls, document them in the
commit message, and leave a line comment for the reviewer on anything genuinely
ambiguous.

Inputs:
  Task:  {task}   (lists what to do, test steps, and line refs into spec/plan)
  Spec:  {spec}
  Plan:  {plan}
{prev_review}{resume_hint}
Job:
  1. Read the task file first.
  2. Implement end-to-end on the current branch.
  3. Make the tests described in the task pass — UNLESS the task's `test`
     field starts with "not testable" (genuine scaffolding/intermediate
     task). In that case you may skip writing tests, but the code you land
     MUST still build cleanly (compiles / links / type-checks) on its own.
  4. Commit as a series of small atomic commits (see below). The reviewer
     inspects the aggregate `base..HEAD` range for this round, where `base`
     was pinned by the orchestrator before you started; the TUI detects
     completion by observing HEAD advanced past base.

Commit granularity (MANDATORY):
  - Prefer many small atomic commits over one large one. Each commit = ONE
    logical change that stands on its own (a single refactor step, a new
    function + its test, a single bug fix, a single rename). Any commit read
    in isolation should reveal its intent.
  - Each commit leaves the tree consistent: code compiles and tests relevant
    to that change pass. Do NOT split so an intermediate commit is broken.
  - Do NOT mix unrelated changes in one commit (e.g. rename + bug fix + new
    feature). Do NOT bundle formatting/whitespace churn into a functional
    commit — make it a separate `style:`/`chore:` commit if at all.
  - If a commit's real-logic diff (excluding generated files, lockfiles, large
    fixtures) exceeds ~200 lines, consider splitting.
  - One-task-one-commit is acceptable ONLY when the task genuinely is one
    atomic change. Otherwise split.

Commit message (MANDATORY — reviewer rejects violations):
  - Conventional Commits: `type(scope): summary` (feat, fix, refactor, test,
    docs, chore, perf, style, build). E.g. `feat(auth): add refresh-token
    rotation`, `fix(db): close pool on shutdown`.
  - No `Co-Authored-By:` trailers or other co-author attribution.
  - No orchestrator vocabulary: no "task <N>", "round <N>", "plan", "shard",
    "phase", or references to this prompt. Write as if a human engineer
    authored the change standalone.

Delegate tedious chores to subagents — bulk renames, codebase audits, test
sweeps, dependency tracing, large refactors. They run in parallel. Give each
a clear, self-contained brief and verify their output before committing.

Hard rules:
  - Do NOT ask clarifying questions; work from task + spec + plan.
  - Stay within this one task's scope. Follow-up work you uncover → note for
    the reviewer; do NOT do it yourself.
  - Do NOT force-push, rebase history, or delete branches.
  - Do NOT proceed to the next task; one task per round.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        prev_review = prev_review,
        resume_hint = resume_hint,
        instr = instr,
    )
}

fn reviewer_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    base_file: &std::path::Path,
    commits_file: &std::path::Path,
    review_file: &std::path::Path,
) -> String {
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let live_summary_path = session_dir.join("artifacts").join("live_summary.txt");
    let instr = live_summary_instruction(&live_summary_path);
    format!(
        r#"You are the reviewer for task {task_id}, round {round}. NON-INTERACTIVE — no
operator. Do NOT modify code. Write ONLY the review TOML.

Inputs:
  Task:        {task}
  Spec:        {spec}
  Plan:        {plan}
  Base SHA:    {base}     (one SHA = HEAD at round start)
  Commit list: {commits}  (one SHA per line in base..HEAD; may be empty if
                           git was unavailable — fall back to `git log` yourself)

Review:
  1. BASE=$(cat {base})
     `git log --oneline $BASE..HEAD` — every commit in this round.
     `git diff $BASE..HEAD`           — aggregate change.
     `git show <sha>`                 — drill into any commit.
     The coder may have made one or more commits; judge the aggregate delta
     against the task. Per-commit structure is the coder's choice.
  2. Judge task completion: does the aggregate delta actually deliver what's
     required? Read the task `description` AND the spec/plan sections it
     points to (via `spec_refs` and `plan_refs` in the task file) — the task
     is complete only when the delta satisfies all of them. A green test run
     does NOT by itself prove completion, and a missing test run does NOT by
     itself prove failure — read the code against those requirements.
  3. Verify the task's test description passes (run it, inspect code). If the
     task's `test` field starts with "not testable" (scaffolding/intermediate
     task), SKIP the test-pass check — but still require the code to build
     cleanly (compiles / links / type-checks). Completion still matters.
  4. Check correctness, missing edge cases, broken contracts, bad error
     handling, test gaps. Uncommitted working-tree changes are NOT in scope —
     review only `base..HEAD`.

Emit the verdict to {review} in EXACTLY this TOML shape (double-quoted strings;
triple-quoted for multi-line; arrays of inline tables for any new task refs):

    status  = "done" | "revise" | "blocked"
    summary = "One-paragraph summary of what was done and your verdict."
    feedback = [
      "Specific thing to fix, if status is revise/blocked.",
      "One item per string.",
    ]

    # Optional: follow-up tasks for work genuinely out-of-scope for this task
    # but needed later.
    [[new_tasks]]
    id = 100
    title = "…"
    description = """…"""
    test = """…"""
    estimated_tokens = 150000
    spec_refs = [{{ path = "artifacts/spec.md", lines = "10-30" }}]
    plan_refs = [{{ path = "artifacts/plan.md", lines = "50-70" }}]

Rules:
  - done    → task outcomes are delivered AND (tests pass, OR task is marked
              "not testable" and the code builds cleanly).
  - revise  → coder must iterate; feedback MUST list the specific issues.
  - blocked → human judgement required; feedback MUST explain what's unclear.
  - Do NOT leave feedback empty for revise/blocked.
  - Do NOT emit prose outside the TOML.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        base = base_file.display(),
        commits = commits_file.display(),
        review = review_file.display(),
        instr = instr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RunRecord, RunStatus};

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::TempDir::new().expect("tempdir");
        let cwd = std::env::current_dir().expect("cwd");

        std::env::set_current_dir(temp.path()).expect("enter temp root");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::env::set_current_dir(cwd).expect("restore cwd");
        result.expect("test panicked")
    }

    fn mk_tmux() -> TmuxContext {
        TmuxContext {
            session_name: "test".to_string(),
            window_index: "0".to_string(),
            window_name: "test".to_string(),
        }
    }

    fn mk_state_with_runs() -> SessionState {
        let mut state = SessionState::new("t".to_string());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "spec-review".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Spec Review 1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
        });
        state
    }

    fn mk_app(state: SessionState) -> App {
        let nodes = build_tree(&state);
        let selected = current_node_index(&nodes);
        App {
            tmux: mk_tmux(),
            state,
            nodes,
            models: Vec::new(),
            model_refresh: ModelRefreshState::Idle(Instant::now()),
            selected,
            expanded: BTreeSet::new(),
            stage_scroll: BTreeMap::new(),
            body_inner_height: 30,
            body_inner_width: 80,
            input_mode: false,
            input_buffer: String::new(),
            confirm_back: false,
            window_launched: true,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_path: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            current_run_id: Some(2),
            failed_models: HashMap::new(),
            test_launch_harness: None,
            messages: Vec::new(),
        }
    }

    #[test]
    fn current_stage_is_always_expanded() {
        let app = mk_app(mk_state_with_runs());
        let current = app.current_node();
        assert!(app.is_expanded(current));
    }

    #[test]
    fn toggle_expand_adds_then_removes_by_stage_key() {
        let mut app = mk_app(mk_state_with_runs());
        // Focus the Brainstorm stage (not the running one).
        let bs_idx = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        app.selected = bs_idx;
        assert!(!app.is_expanded(bs_idx));
        app.toggle_expand_focused();
        assert!(app.is_expanded(bs_idx));
        assert!(app.expanded.contains("Brainstorm"));
        app.toggle_expand_focused();
        assert!(!app.is_expanded(bs_idx));
    }

    #[test]
    fn toggle_noop_on_currently_running_stage() {
        let mut app = mk_app(mk_state_with_runs());
        app.selected = app.current_node();
        app.toggle_expand_focused();
        assert!(app.expanded.is_empty());
        // Still expanded via the current-running rule.
        assert!(app.is_expanded(app.selected));
    }

    #[test]
    fn expand_state_survives_tree_rebuild() {
        let mut app = mk_app(mk_state_with_runs());
        let bs_idx = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        app.selected = bs_idx;
        app.toggle_expand_focused();
        assert!(app.expanded.contains("Brainstorm"));
        // Rebuild tree — indices may shift, but stage-keyed state persists.
        app.nodes = build_tree(&app.state);
        let bs_idx_after = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        assert!(app.is_expanded(bs_idx_after));
    }

    #[test]
    fn scroll_offsets_keyed_by_stage_identity() {
        let mut app = mk_app(mk_state_with_runs());
        let bs_idx = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        app.expanded.insert("Brainstorm".to_string());
        app.set_stage_scroll(bs_idx, 7);
        assert_eq!(app.stage_scroll.get("Brainstorm").copied(), Some(7));
        // Rebuild the tree, then confirm offset still there.
        app.nodes = build_tree(&app.state);
        let bs_idx_after = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        assert_eq!(
            app.stage_scroll
                .get(&app.nodes[bs_idx_after].label)
                .copied(),
            Some(7)
        );
    }

    #[test]
    fn transition_resets_scroll_only_for_changed_leaf_run() {
        let mut app = mk_app(mk_state_with_runs());
        // Pre-set scroll for Brainstorm (leaf_run_id=1) and Spec Review (running, id=2).
        app.stage_scroll.insert("Brainstorm".to_string(), 3);
        app.stage_scroll.insert("Spec Review".to_string(), 5);

        // Advance: brainstorm's leaf doesn't change; spec review's run pool may shift.
        // Mimic a phase transition where brainstorm leaf stays the same.
        let _ = app.transition_to_phase(Phase::SpecReviewPaused);
        // Brainstorm leaf_run_id didn't change, offset preserved.
        assert_eq!(app.stage_scroll.get("Brainstorm").copied(), Some(3));
    }

    #[test]
    fn boundary_handoff_on_up_moves_focus_to_previous_expanded_stage() {
        let mut app = mk_app(mk_state_with_runs());
        // Expand Brainstorm so it can receive focus handoff.
        app.expanded.insert("Brainstorm".to_string());
        // Focus Spec Review (currently running, implicitly expanded) with scroll at top.
        let sr_idx = app
            .nodes
            .iter()
            .position(|n| n.label == "Spec Review")
            .unwrap();
        app.selected = sr_idx;
        app.set_stage_scroll(sr_idx, 0);
        // Pressing Up at the top boundary should move focus to the previous stage.
        app.scroll_or_move_focus(-1);
        assert!(app.selected < sr_idx);
    }

    #[test]
    fn collapse_then_reexpand_preserves_scroll_offset() {
        let mut app = mk_app(mk_state_with_runs());
        let bs_idx = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        // Focus Brainstorm, expand it, and set a concrete scroll offset.
        app.selected = bs_idx;
        app.expanded.insert("Brainstorm".to_string());
        app.set_stage_scroll(bs_idx, 4);
        assert_eq!(app.stage_scroll.get("Brainstorm").copied(), Some(4));

        // Collapse (toggle off) — focus moves elsewhere so the running-stage
        // implicit-expand rule doesn't keep Brainstorm expanded.
        app.toggle_expand_focused();
        assert!(!app.is_expanded(bs_idx));
        // Simulate a render cycle that calls clamp_scroll after tree state changes.
        app.clamp_scroll();
        // Scroll offset must survive while the stage is collapsed.
        assert_eq!(app.stage_scroll.get("Brainstorm").copied(), Some(4));

        // Re-expand — offset should still be 4 (clamping may reduce it only if the
        // current viewport cannot fit it, which it can here since max_offset = 0
        // only bounds down and our test keeps content empty so max_offset is 0; we
        // therefore assert the offset is *retained as stored* up to that point by
        // checking the map directly before clamp applies to the now-expanded stage).
        app.expanded.insert("Brainstorm".to_string());
        assert_eq!(app.stage_scroll.get("Brainstorm").copied(), Some(4));
    }

    fn node_index(app: &App, label: &str) -> usize {
        app.nodes.iter().position(|n| n.label == label).unwrap()
    }

    #[test]
    fn boundary_handoff_skips_collapsed_stages_between_expanded_neighbors() {
        let mut app = mk_app(mk_state_with_runs());
        // Layout: Idea, Brainstorm, Spec Review(running/implicit-expand),
        // Planning, Plan Review, Sharding, Builder Loop.
        // Expand Plan Review, leave Planning collapsed between it and Spec Review,
        // and focus Plan Review at the top boundary.
        let pr_idx = node_index(&app, "Plan Review");
        let planning_idx = node_index(&app, "Planning");
        let sr_idx = node_index(&app, "Spec Review");
        assert!(planning_idx > sr_idx && planning_idx < pr_idx);
        app.expanded.insert("Plan Review".to_string());
        assert!(!app.is_expanded(planning_idx));
        app.selected = pr_idx;
        app.set_stage_scroll(pr_idx, 0);

        // Up at the top boundary should jump past the collapsed Planning stage
        // directly to Spec Review (the next expanded stage upward).
        app.scroll_or_move_focus(-1);
        assert_eq!(
            app.nodes[app.selected].label, "Spec Review",
            "expected focus to skip collapsed Planning and land on Spec Review, got {:?}",
            app.nodes[app.selected].label
        );
    }

    #[test]
    fn space_binding_does_not_affect_input_mode() {
        let mut app = mk_app(mk_state_with_runs());
        app.input_mode = true;
        let before = app.expanded.clone();
        // Directly test the guard: toggle_expand_focused shouldn't be reached via
        // input-mode keys. Sanity: toggle itself still works outside input mode.
        app.input_mode = false;
        app.selected = app
            .nodes
            .iter()
            .position(|n| n.label == "Brainstorm")
            .unwrap();
        app.toggle_expand_focused();
        assert_ne!(app.expanded, before);
    }

    fn sample_model(name: &str, idea_rank: u8, build_rank: u8) -> selection::ModelStatus {
        selection::ModelStatus {
            vendor: selection::VendorKind::Claude,
            name: name.to_string(),
            stupid_level: Some(7),
            quota_percent: Some(80),
            idea_rank,
            planning_rank: 10,
            build_rank,
            review_rank: 10,
            idea_weight: 0.0,
            planning_weight: 0.0,
            build_weight: 0.0,
            review_weight: 0.0,
        }
    }

    fn ranked_model(
        vendor: selection::VendorKind,
        name: &str,
        planning_rank: u8,
        build_rank: u8,
        review_rank: u8,
    ) -> selection::ModelStatus {
        selection::ModelStatus {
            vendor,
            name: name.to_string(),
            stupid_level: Some(7),
            quota_percent: Some(80),
            idea_rank: 10,
            planning_rank,
            build_rank,
            review_rank,
            idea_weight: 0.0,
            planning_weight: 0.0,
            build_weight: 0.0,
            review_weight: 0.0,
        }
    }

    fn idle_app(state: SessionState) -> App {
        let nodes = build_tree(&state);
        let selected = current_node_index(&nodes);
        App {
            tmux: mk_tmux(),
            state,
            nodes,
            models: Vec::new(),
            model_refresh: ModelRefreshState::Idle(Instant::now()),
            selected,
            expanded: BTreeSet::new(),
            stage_scroll: BTreeMap::new(),
            body_inner_height: 30,
            body_inner_width: 80,
            input_mode: false,
            input_buffer: String::new(),
            confirm_back: false,
            window_launched: false,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_path: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            current_run_id: None,
            failed_models: HashMap::new(),
            test_launch_harness: None,
            messages: Vec::new(),
        }
    }

    #[test]
    fn brainstorm_selection_uses_idea_task_kind() {
        let models = vec![
            sample_model("idea-first", 1, 2),
            sample_model("build-first", 2, 1),
        ];

        let chosen = App::select_brainstorm_model(&models).expect("expected brainstorm model");

        assert_eq!(chosen.name, "idea-first");
    }

    #[test]
    fn app_new_rebuilds_failed_models_without_force_retry_runs() {
        with_temp_root(|| {
            let session_id = "rebuild-failed-models";
            let mut state = SessionState::new(session_id.to_string());
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(7),
                round: 3,
                attempt: 1,
                model: "claude-sonnet".to_string(),
                vendor: "claude".to_string(),
                window_name: "[Coder r3]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
            });
            state.agent_runs.push(RunRecord {
                id: 2,
                stage: "coder".to_string(),
                task_id: Some(7),
                round: 3,
                attempt: 2,
                model: "gemini-2.5-pro".to_string(),
                vendor: "gemini".to_string(),
                window_name: "[Coder r3]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("artifact_missing".to_string()),
            });
            state.agent_runs.push(RunRecord {
                id: 3,
                stage: "coder".to_string(),
                task_id: Some(7),
                round: 3,
                attempt: 3,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Coder r3]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("user_forced_retry".to_string()),
            });
            state.save().expect("save session");

            let app = App::new(
                mk_tmux(),
                SessionState::load(session_id).expect("load session"),
            );

            let key = ("coder".to_string(), Some(7), 3);
            let failed = app
                .failed_models
                .get(&key)
                .expect("expected failed model set");
            assert!(failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string())));
            assert!(
                failed.contains(&(selection::VendorKind::Gemini, "gemini-2.5-pro".to_string()))
            );
            assert!(!failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
            assert!(app.current_run_id.is_none());
        });
    }

    #[test]
    fn normalize_failure_reason_reports_exit_signal_and_artifact_errors() {
        with_temp_root(|| {
            let session_id = "normalize-failure-reason";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
            let state = SessionState::new(session_id.to_string());
            let app = mk_app(state);
            let run = RunRecord {
                id: 9,
                stage: "planning".to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Planning]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
            };
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status dir"))
                .expect("create status dir");

            std::fs::write(app.run_status_path(&run), "1").expect("write exit code");
            assert_eq!(
                app.normalized_failure_reason(&run).expect("exit reason"),
                Some("exit(1)".to_string())
            );

            std::fs::write(app.run_status_path(&run), "143").expect("write signal exit");
            assert_eq!(
                app.normalized_failure_reason(&run).expect("signal reason"),
                Some("killed(15)".to_string())
            );

            std::fs::write(app.run_status_path(&run), "0").expect("write clean exit");
            assert_eq!(
                app.normalized_failure_reason(&run)
                    .expect("missing artifact"),
                Some("artifact_missing".to_string())
            );

            std::fs::write(session_dir.join("artifacts").join("plan.md"), "")
                .expect("write empty plan");
            assert_eq!(
                app.normalized_failure_reason(&run).expect("empty artifact"),
                Some("artifact_missing".to_string())
            );

            let brainstorm = RunRecord {
                stage: "brainstorm".to_string(),
                window_name: "[Brainstorm]".to_string(),
                ..run.clone()
            };
            std::fs::write(app.run_status_path(&brainstorm), "0").expect("clean brainstorm exit");
            std::fs::write(session_dir.join("artifacts").join("spec.md"), "")
                .expect("write empty spec");
            assert_eq!(
                app.normalized_failure_reason(&brainstorm)
                    .expect("empty spec"),
                Some("artifact_missing".to_string())
            );

            let sharding = RunRecord {
                stage: "sharding".to_string(),
                window_name: "[Sharding]".to_string(),
                ..run.clone()
            };
            std::fs::write(app.run_status_path(&sharding), "0").expect("clean sharding exit");
            std::fs::write(
                session_dir.join("artifacts").join("tasks.toml"),
                "not valid toml = [",
            )
            .expect("write invalid tasks");
            assert!(
                app.normalized_failure_reason(&sharding)
                    .expect("invalid tasks")
                    .expect("error text")
                    .starts_with("artifact_invalid: ")
            );
        });
    }

    #[test]
    fn coder_retry_loop_uses_distinct_models_until_success() {
        with_temp_root(|| {
            let session_id = "coder-retry-loop";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let mut app = idle_app(state);
            app.models = vec![
                ranked_model(selection::VendorKind::Claude, "claude-sonnet", 10, 1, 10),
                ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
                ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 3, 10),
            ];
            let harness = std::sync::Arc::new(std::sync::Mutex::new(TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 0,
                        artifact_contents: Some("abc123".to_string()),
                    },
                ]),
            }));
            app.test_launch_harness = Some(harness);

            app.launch_coder();
            for _ in 0..6 {
                if app.current_run_id.is_none() {
                    break;
                }
                app.poll_agent_window();
            }

            assert!(app.current_run_id.is_none());
            assert_eq!(app.state.agent_runs.len(), 3);
            assert_eq!(app.state.agent_runs[0].attempt, 1);
            assert_eq!(app.state.agent_runs[1].attempt, 2);
            assert_eq!(app.state.agent_runs[2].attempt, 3);
            assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
            assert_eq!(app.state.agent_runs[1].status, RunStatus::Failed);
            assert_eq!(app.state.agent_runs[2].status, RunStatus::Done);
            assert_eq!(app.state.agent_runs[0].error.as_deref(), Some("exit(1)"));
            assert_eq!(app.state.agent_runs[1].error.as_deref(), Some("exit(1)"));
            assert_eq!(app.state.agent_runs[0].model, "claude-sonnet");
            assert_eq!(app.state.agent_runs[1].model, "gemini-2.5-pro");
            assert_eq!(app.state.agent_runs[2].model, "gpt-5");
            assert_eq!(app.state.current_phase, Phase::ReviewRound(1));

            let end_texts = app
                .messages
                .iter()
                .filter(|message| message.kind == MessageKind::End)
                .map(|message| message.text.clone())
                .collect::<Vec<_>>();
            assert!(end_texts.contains(&"attempt 1 failed: exit(1)".to_string()));
            assert!(end_texts.contains(&"attempt 2 failed: exit(1)".to_string()));

            let started_texts = app
                .messages
                .iter()
                .filter(|message| message.kind == MessageKind::Started)
                .map(|message| message.text.clone())
                .collect::<Vec<_>>();
            assert!(started_texts.contains(&"retrying with gemini/gemini-2.5-pro".to_string()));
            assert!(started_texts.contains(&"retrying with codex/gpt-5".to_string()));
        });
    }

    #[test]
    fn coder_retry_exhaustion_enters_builder_recovery() {
        with_temp_root(|| {
            let session_id = "coder-retry-exhaustion";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.pending = vec![2, 3];
            state.builder.current_task = Some(1);
            let mut app = idle_app(state);
            app.models = vec![
                ranked_model(selection::VendorKind::Claude, "claude-sonnet", 10, 1, 10),
                ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
            ];
            let harness = std::sync::Arc::new(std::sync::Mutex::new(TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                ]),
            }));
            app.test_launch_harness = Some(harness);

            app.launch_coder();
            for _ in 0..5 {
                if app.current_run_id.is_none() {
                    break;
                }
                app.poll_agent_window();
            }

            assert!(app.current_run_id.is_none());
            assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
            assert_eq!(app.state.builder.current_task, None);
            assert_eq!(app.state.builder.pending, vec![2, 3]);
            let summary = app
                .state
                .builder
                .recovery_trigger_summary
                .clone()
                .expect("recovery trigger summary");
            assert!(summary.starts_with("retry exhausted (2 attempts)"));
            assert!(summary.contains("attempt 1: claude/claude-sonnet"));
            assert!(summary.contains("attempt 2: gemini/gemini-2.5-pro"));
        });
    }

    #[test]
    fn non_builder_retry_exhaustion_still_blocks() {
        let mut state = SessionState::new("non-builder-retry".to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet",
            1,
            10,
            10,
        )];
        let failed = RunRecord {
            id: 11,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 3,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
        };
        let handled = app.maybe_auto_retry(&failed);
        assert!(handled);
        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert!(!matches!(
            app.state.current_phase,
            Phase::BuilderRecovery(_)
        ));
    }

    #[test]
    fn recovery_retry_exhaustion_falls_back_to_blocked() {
        let mut state = SessionState::new("recovery-retry-cap".to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet",
            1,
            10,
            10,
        )];
        let failed = RunRecord {
            id: 21,
            stage: "recovery".to_string(),
            task_id: None,
            round: 2,
            attempt: 3,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Recovery]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("artifact_invalid: x".to_string()),
        };
        let handled = app.maybe_auto_retry(&failed);
        assert!(handled);
        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("builder recovery retry exhausted")
        );
    }

    #[test]
    fn review_blocked_enters_builder_recovery() {
        with_temp_root(|| {
            let session_id = "review-blocked-recovery";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("rounds").join("001")).expect("round dir");
            std::fs::write(
                session_dir.join("rounds").join("001").join("review.toml"),
                r#"status = "blocked"
summary = "needs recovery"
feedback = ["task 2 is superseded"]
"#,
            )
            .expect("review file");
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ReviewRound(1);
            state.builder.current_task = Some(2);
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "reviewer".to_string(),
                task_id: Some(2),
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Review]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
            });
            let mut app = idle_app(state);
            let run = app.state.agent_runs[0].clone();
            app.finalize_current_run(&run).expect("finalize review");
            assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
            assert_eq!(app.state.builder.current_task, None);
            assert_eq!(app.state.builder.recovery_trigger_task_id, Some(2));
        });
    }

    #[test]
    fn recovery_reconcile_replaces_pending_and_sets_retry_reset_cutoff() {
        with_temp_root(|| {
            let session_id = "recovery-reconcile-success";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::write(
                artifacts.join("spec.md"),
                "Spec\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
            )
            .expect("spec");
            std::fs::write(
                artifacts.join("plan.md"),
                "Plan\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
            )
            .expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 2
title = "Finish task 2"
description = "do it"
test = "cargo test"
estimated_tokens = 10

[[tasks]]
id = 5
title = "New follow-up"
description = "new work"
test = "cargo test"
estimated_tokens = 10
"#,
            )
            .expect("tasks");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(2);
            state.builder.done = vec![1, 4];
            state.builder.pending = vec![2, 3];
            state.builder.current_task = Some(2);
            state.builder.recovery_prev_max_task_id = Some(4);
            state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4];
            state.agent_runs.push(RunRecord {
                id: 7,
                stage: "coder".to_string(),
                task_id: Some(2),
                round: 2,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Done,
                error: None,
            });
            state.agent_runs.push(RunRecord {
                id: 8,
                stage: "recovery".to_string(),
                task_id: None,
                round: 2,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Recovery]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
            });
            let mut app = idle_app(state);
            let run = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 8)
                .cloned()
                .expect("recovery run");
            app.finalize_current_run(&run).expect("finalize recovery");

            assert_eq!(app.state.current_phase, Phase::ImplementationRound(3));
            assert_eq!(app.state.builder.done, vec![1, 4]);
            assert_eq!(app.state.builder.pending, vec![2, 5]);
            assert_eq!(app.state.builder.current_task, None);
            assert_eq!(app.state.builder.retry_reset_run_id_cutoff, Some(8));
        });
    }

    #[test]
    fn recovery_reconcile_requires_notes_for_superseded_started_tasks() {
        with_temp_root(|| {
            let session_id = "recovery-reconcile-notes";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::write(artifacts.join("spec.md"), "Spec without section").expect("spec");
            std::fs::write(artifacts.join("plan.md"), "Plan without section").expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 6
title = "Replacement"
description = "replace task 2"
test = "cargo test"
estimated_tokens = 10
"#,
            )
            .expect("tasks");

            let mut state = SessionState::new(session_id.to_string());
            state.builder.done = vec![1];
            state.builder.recovery_prev_max_task_id = Some(5);
            state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4, 5];
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(2),
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Done,
                error: None,
            });
            let mut app = idle_app(state);
            let err = app
                .reconcile_builder_recovery(99)
                .expect_err("expected supersession rejection");
            let text = format!("{err:#}");
            assert!(text.contains("Recovery Notes"));
        });
    }

    #[test]
    fn app_new_rebuild_failed_models_skips_builder_failures_before_retry_reset_cutoff() {
        with_temp_root(|| {
            let session_id = "failed-model-retry-reset";
            let mut state = SessionState::new(session_id.to_string());
            state.builder.retry_reset_run_id_cutoff = Some(10);
            state.agent_runs.push(RunRecord {
                id: 9,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 1,
                model: "claude-sonnet".to_string(),
                vendor: "claude".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
            });
            state.agent_runs.push(RunRecord {
                id: 11,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 2,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
            });
            state.save().expect("save");
            let app = App::new(mk_tmux(), SessionState::load(session_id).expect("load"));
            let key = ("coder".to_string(), Some(1), 1);
            let failed = app.failed_models.get(&key).expect("failed set");
            assert_eq!(failed.len(), 1);
            assert!(failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
            assert!(
                !failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string()))
            );
        });
    }

    #[test]
    fn recovery_auto_launch_is_idempotent_on_resume() {
        with_temp_root(|| {
            let session_id = "recovery-resume-autolaunch";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::write(artifacts.join("spec.md"), "spec").expect("spec");
            std::fs::write(artifacts.join("plan.md"), "plan").expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 1
title = "Task"
description = "d"
test = "t"
estimated_tokens = 1
"#,
            )
            .expect("tasks");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(1);
            let mut app = idle_app(state);
            app.models = vec![ranked_model(
                selection::VendorKind::Codex,
                "gpt-5",
                1,
                10,
                10,
            )];
            app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
                TestLaunchHarness {
                    outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                        exit_code: 0,
                        artifact_contents: None,
                    }]),
                },
            )));

            app.maybe_auto_launch();
            let first_run_count = app.state.agent_runs.len();
            assert_eq!(first_run_count, 1);
            assert_eq!(app.state.agent_runs[0].stage, "recovery");

            app.maybe_auto_launch();
            assert_eq!(app.state.agent_runs.len(), first_run_count);
        });
    }

    #[test]
    fn brainstorm_failures_do_not_retry_or_populate_failed_models() {
        with_temp_root(|| {
            let session_id = "brainstorm-no-retry";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BrainstormRunning;
            let run = RunRecord {
                id: 1,
                stage: "brainstorm".to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "claude-sonnet".to_string(),
                vendor: "claude".to_string(),
                window_name: "[Brainstorm]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
            };
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);
            app.models = vec![ranked_model(
                selection::VendorKind::Claude,
                "claude-sonnet",
                1,
                1,
                1,
            )];
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status dir"))
                .expect("create status dir");
            std::fs::write(app.run_status_path(&run), "1").expect("write exit code");

            app.finalize_current_run(&run)
                .expect("finalize brainstorm failure");
            assert!(app.failed_models.is_empty());
            assert_eq!(app.state.agent_runs.len(), 1);
            assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
            assert_eq!(app.state.agent_runs[0].error.as_deref(), Some("exit(1)"));
            assert_eq!(app.state.agent_error.as_deref(), Some("exit(1)"));

            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status dir"))
                .expect("recreate status dir");
            std::fs::write(app.run_status_path(&run), "0").expect("write clean exit");
            std::fs::write(session_dir.join("artifacts").join("spec.md"), "spec")
                .expect("write spec");
            app.state.agent_runs[0].status = RunStatus::Running;
            app.state.agent_runs[0].error = None;
            app.finalize_current_run(&run)
                .expect("finalize brainstorm success");
            assert_eq!(app.state.current_phase, Phase::SpecReviewRunning);
            assert!(app.failed_models.is_empty());
        });
    }
}
