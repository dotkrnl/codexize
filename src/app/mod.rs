mod events;
mod models;
mod render;
mod state;
mod tree;

use crate::{
    adapters::{AgentRun, adapter_for_vendor, launch_interactive, launch_noninteractive},
    cache, review,
    selection::{self, ModelStatus, QuotaError, select_for_review},
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
    collections::{BTreeMap, BTreeSet},
    sync::mpsc,
    time::{Duration, Instant},
};

const PREVIEW_LINES: usize = 3;

#[derive(Debug)]
pub struct App {
    tmux: TmuxContext,
    state: SessionState,
    nodes: Vec<Node>,
    models: Vec<ModelStatus>,
    model_refresh: ModelRefreshState,
    selected: usize,
    expanded: BTreeSet<usize>,
    node_scroll: Vec<usize>,
    body_inner_height: usize,
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
    messages: Vec<Message>,
}

impl App {
    pub fn new(tmux: TmuxContext, state: SessionState) -> Self {
        let messages = SessionState::load_messages(&state.session_id).unwrap_or_default();
        let nodes = build_tree(&state);
        let node_count = nodes.len();
        let current = current_node_index(&nodes);
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
            node_scroll: vec![usize::MAX; node_count],
            body_inner_height: 0,
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

    fn is_expanded(&self, index: usize) -> bool {
        index == self.current_node() || self.expanded.contains(&index)
    }

    fn page_step(&self) -> usize {
        self.selected_body_limit().saturating_sub(2).max(1)
    }

    fn selected_body_limit(&self) -> usize {
        let expanded_preview_count = self
            .expanded
            .iter()
            .filter(|index| **index != self.selected)
            .count();
        let reserved = self.nodes.len() + expanded_preview_count * PREVIEW_LINES;
        self.body_inner_height.saturating_sub(reserved).max(6)
    }

    fn node_scroll_offset(&self, index: usize, total: usize, limit: usize) -> usize {
        let max_offset = total.saturating_sub(limit);
        if self.node_scroll[index] == usize::MAX {
            max_offset
        } else {
            self.node_scroll[index].min(max_offset)
        }
    }

    fn stage_scroll_key(node: &Node) -> Option<String> {
        if node.kind != session_state::NodeKind::Stage {
            return None;
        }
        // REVIEWER: stage labels are currently unique in the tree; if that changes,
        // this key should include task_id/round identity to avoid collisions.
        Some(node.label.clone())
    }

    fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        let previous_stage_leaf_runs: BTreeMap<String, Option<u64>> = self
            .nodes
            .iter()
            .filter_map(|node| Self::stage_scroll_key(node).map(|key| (key, node.leaf_run_id)))
            .collect();
        let previous_stage_scrolls: BTreeMap<String, usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                Self::stage_scroll_key(node).map(|key| {
                    let scroll = self.node_scroll.get(index).copied().unwrap_or(usize::MAX);
                    (key, scroll)
                })
            })
            .collect();

        self.state.transition_to(next_phase)?;
        self.agent_line_count = 0;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;

        self.nodes = build_tree(&self.state);
        self.node_scroll = self
            .nodes
            .iter()
            .map(|node| {
                let Some(key) = Self::stage_scroll_key(node) else {
                    return usize::MAX;
                };
                let previous_leaf = previous_stage_leaf_runs.get(&key).copied();
                let previous_scroll = previous_stage_scrolls
                    .get(&key)
                    .copied()
                    .unwrap_or(usize::MAX);
                if previous_leaf == Some(node.leaf_run_id) {
                    previous_scroll
                } else {
                    usize::MAX
                }
            })
            .collect();
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
        self.node_scroll.resize(self.nodes.len(), usize::MAX);
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
        if tmux::window_exists(&run.window_name) {
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
        self.node_scroll.resize(self.nodes.len(), usize::MAX);
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
                "failed in {minutes}m{seconds:02}s: {}",
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

    fn finalize_current_run(&mut self, run: &crate::state::RunRecord) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                if spec_path.exists() {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    self.transition_to_phase(Phase::SpecReviewRunning)?;
                } else {
                    let error = "missing spec artifact".to_string();
                    self.finalize_run_record(run.id, false, Some(error.clone()));
                    self.state.agent_error = Some(error);
                }
            }
            Phase::SpecReviewRunning => {
                let round = run.round;
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{round}.md"));
                if review_path.exists() {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    self.transition_to_phase(Phase::SpecReviewPaused)?;
                } else {
                    let error = format!("missing {}", review_path.display());
                    self.finalize_run_record(run.id, false, Some(error.clone()));
                    self.state.agent_error = Some(error);
                }
            }
            Phase::PlanningRunning => {
                let plan_path = session_dir.join("artifacts").join("plan.md");
                if plan_path.exists() {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    self.transition_to_phase(Phase::PlanReviewRunning)?;
                } else {
                    let error = "missing plan artifact".to_string();
                    self.finalize_run_record(run.id, false, Some(error.clone()));
                    self.state.agent_error = Some(error);
                }
            }
            Phase::PlanReviewRunning => {
                let round = run.round;
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("plan-review-{round}.md"));
                if review_path.exists() {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    self.transition_to_phase(Phase::PlanReviewPaused)?;
                } else {
                    let error = format!("missing {}", review_path.display());
                    self.finalize_run_record(run.id, false, Some(error.clone()));
                    self.state.agent_error = Some(error);
                }
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
                    Err(err) => {
                        let error = err.to_string();
                        self.finalize_run_record(run.id, false, Some(error.clone()));
                        self.state.agent_error = Some(error);
                    }
                }
            }
            Phase::ImplementationRound(round) => {
                let commit_path = session_dir
                    .join("rounds")
                    .join(format!("{round:03}"))
                    .join("commit.txt");
                if commit_path.exists() {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    self.transition_to_phase(Phase::ReviewRound(round))?;
                } else {
                    let error = format!("missing {}", commit_path.display());
                    self.finalize_run_record(run.id, false, Some(error.clone()));
                    self.state.agent_error = Some(error);
                }
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
                                self.transition_to_phase(Phase::BlockedNeedsUser)?;
                            }
                        }
                    }
                    Err(err) => {
                        let error = err.to_string();
                        self.finalize_run_record(run.id, false, Some(error.clone()));
                        self.state.agent_error = Some(error);
                    }
                }
            }
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

        let Some(chosen) = selection::select(&self.models, selection::TaskKind::Build)
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

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_interactive("[Brainstorm]", &run, adapter.as_ref(), true) {
            Ok(()) => {
                self.state.idea_text = Some(idea.clone());
                self.state.selected_model = Some(model.clone());
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
                self.start_run_tracking(
                    "brainstorm",
                    None,
                    1,
                    model,
                    vendor,
                    "[Brainstorm]".to_string(),
                );
            }
            Err(e) => {
                self.state.agent_error = Some(format!("failed to launch brainstorm: {e}"));
            }
        }
    }

    fn launch_spec_review(&mut self) {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
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

        let Some(chosen) = select_for_review(
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
        .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string())) else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return;
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
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let adapter = adapter_for_vendor(vendor_kind);
        let window_name = format!("[Spec Review {round}]");
        match launch_noninteractive(&window_name, &run, adapter.as_ref()) {
            Ok(()) => {
                self.start_run_tracking("spec-review", None, round, model, vendor, window_name);
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch spec review: {err}"));
            }
        }
    }

    fn launch_planning(&mut self) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
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

        let Some(chosen) = selection::select(&self.models, selection::TaskKind::Planning)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
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
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_interactive("[Planning]", &run, adapter.as_ref(), true) {
            Ok(()) => {
                self.start_run_tracking(
                    "planning",
                    None,
                    1,
                    model,
                    vendor,
                    "[Planning]".to_string(),
                );
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch planning: {e}"));
            }
        }
    }

    fn launch_plan_review(&mut self) {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
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

        let Some(chosen) = select_for_review(
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
        .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string())) else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return;
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
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let adapter = adapter_for_vendor(vendor_kind);
        let window_name = format!("[Plan Review {round}]");
        match launch_noninteractive(&window_name, &run, adapter.as_ref()) {
            Ok(()) => {
                self.start_run_tracking("plan-review", None, round, model, vendor, window_name);
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch plan review: {err}"));
            }
        }
    }

    fn launch_sharding(&mut self) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
        }

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");

        let Some(chosen) = selection::select(&self.models, selection::TaskKind::Planning)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
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
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive("[Sharding]", &run, adapter.as_ref()) {
            Ok(()) => {
                self.start_run_tracking(
                    "sharding",
                    None,
                    1,
                    model,
                    vendor,
                    "[Sharding]".to_string(),
                );
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch sharding: {e}"));
            }
        }
    }

    fn launch_coder(&mut self) {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
        }
        let Phase::ImplementationRound(r) = self.state.current_phase else {
            return;
        };

        let Some(task_id) = self.ensure_builder_task_for_round(r) else {
            self.state.agent_error = Some("no pending tasks".to_string());
            let _ = self.state.save();
            return;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let task_file = round_dir.join("task.md");
        let commit_file = round_dir.join("commit.txt");

        if !task_file.exists() {
            let body = task_body_for(&session_dir, task_id).unwrap_or_else(|e| {
                format!("(task body could not be loaded: {e})\n\nTask id: {task_id}\n")
            });
            let _ = std::fs::write(&task_file, body);
        }

        let _ = std::fs::remove_file(&commit_file);

        let Some(chosen) = selection::select(&self.models, selection::TaskKind::Build)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            return;
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
        let prompt = coder_prompt(&session_dir, task_id, r, &task_file, &commit_file, resume);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive(&format!("[Coder r{r}]"), &run, adapter.as_ref()) {
            Ok(()) => {
                self.start_run_tracking(
                    "coder",
                    Some(task_id),
                    r,
                    model,
                    vendor,
                    format!("[Coder r{r}]"),
                );
            }
            Err(e) => {
                let _ = self.state.log_event(format!("failed to launch coder: {e}"));
            }
        }
    }

    fn launch_reviewer(&mut self) {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.nodes = build_tree(&self.state);
            return;
        }
        let Phase::ReviewRound(r) = self.state.current_phase else {
            return;
        };
        let Some(task_id) = self.state.builder.current_task else {
            self.state.agent_error = Some("no current task".to_string());
            let _ = self.state.save();
            return;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let review_path = round_dir.join("review.toml");
        let commit_file = round_dir.join("commit.txt");
        let task_file = round_dir.join("task.md");

        let _ = std::fs::remove_file(&review_path);

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
        let Some(chosen) = select_for_review(&self.models, &excluded)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return;
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
            &commit_file,
            &review_path,
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive(&format!("[Review r{r}]"), &run, adapter.as_ref()) {
            Ok(()) => {
                self.start_run_tracking(
                    "reviewer",
                    Some(task_id),
                    r,
                    model,
                    vendor,
                    format!("[Review r{r}]"),
                );
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch reviewer: {e}"));
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

fn kill_window(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-window", "-t", name])
        .output();
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
        "\n\nPeriodically write a concise plain-text summary of your current progress \
         and next intended action to:\n  {}\nUpdate this file every 2–3 minutes or \
         whenever your major sub-goal changes. One paragraph is enough.\n",
        path.display()
    )
}

fn spec_review_prompt(spec_path: &str, review_path: &str, live_summary_path: &str) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"You are reviewing a spec written by another agent. This is a NON-INTERACTIVE run —
the operator is NOT available. Do not ask clarifying questions; make your best
judgement based only on the spec. Do NOT modify any code in the repository; write
ONLY the review file.

Read the spec at:
{spec_path}

Your task:
1. Read the spec carefully.
2. Evaluate: is the spec clear, complete, and buildable? What risks or gaps do you see?
3. Write your review to: {review_path}

The review must cover:
- Overall verdict (approve / approve-with-changes / reject)
- Specific issues found (if any), each with a suggested fix
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
        r#"You are reviewing an implementation plan written by another agent. This is a
NON-INTERACTIVE run — the operator is NOT available. Do not ask clarifying
questions; make your best judgement.

Read the plan and spec:
  - Plan: {plan_path}
  - Spec: {spec_path}

Your task:
1. Read both files carefully.
2. Review the plan ONLY for CRITICAL issues that would block or break the
   implementation. A critical issue is something like:
     - A spec requirement that has no corresponding plan step (missing work).
     - Plan steps ordered in a way that makes them unbuildable (e.g., a step
       depends on something a later step creates).
     - Contradictions between the plan and spec, or internal contradictions
       that would lead an implementer to build the wrong thing.
     - File paths, function names, or interfaces that are inconsistent
       across steps in a way that would cause real breakage.
     - Spec-level ambiguity so severe that an implementer could not proceed.
   The existence of multiple valid implementations is NOT a plan defect. Do
   not request added detail just to force one internal design choice when
   several reasonable options satisfy the spec and any explicit interfaces.
3. If — and ONLY if — you find critical issues, directly edit {plan_path}
   (and {spec_path} if the issue is spec-level) to fix them. Make the
   smallest edit that resolves the problem.
4. Write a changelog to: {review_path}
   The changelog is a markdown bullet list of what you changed and why.
   If you found no critical issues, write a single bullet saying so — do
   not invent issues to fill space.

DO NOT flag or fix:
  - Typos, grammar, wording, or formatting.
  - Style, tone, or structural polish.
  - Missing low-level implementation detail (the implementer figures that
    out — the plan is a plan, not the code).
  - Absence of prescribed helper/function structure.
  - Multiple possible implementation approaches, unless the plan/spec makes
    an explicit interface commitment that is internally contradictory.
  - Hypothetical edge cases the spec does not require.
  - Minor nitpicks, suggestions, or "nice-to-have" improvements.

When in doubt, leave it alone. Over-editing a plan is worse than under-editing.

Rules:
  - Do NOT create or modify any source code files.
  - Do NOT run git commands or modify version control state.
  - Do NOT ask questions or request operator input.
{instr}"#
    )
}

fn brainstorm_prompt(idea: &str, spec_path: &str, live_summary_path: &str) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"Invoke your brainstorming skill now.

The idea to brainstorm:

---
{idea}
---

When the brainstorming skill asks you to write the design doc, write it to:
{spec_path}

IMPORTANT: this is a spec-only phase. Do NOT write or modify any code in the
repository. Your only output should be the spec file. Implementation happens
in a later phase.

HARD RULES — override anything the superpowers / brainstorming skill suggests:
  - Do NOT `git add`, `git commit`, `git stash`, or otherwise change version
    control state. The spec file lives untracked; a later phase commits.
  - Do NOT ask the operator whether to continue, proceed to planning,
    move to the next stage, or run any follow-up skill. When the spec is
    written, STOP and exit. The orchestrator drives stage transitions,
    not you.
  - If the skill offers a "continue to next stage" prompt inline, ignore
    it and exit.

The operator is here and ready to respond to your questions ABOUT THE DESIGN.
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
        r#"Invoke your writing-plans skill now (superpowers:writing-plans).

You are turning an approved spec — plus any spec reviews — into a concrete
implementation plan.

Inputs to read first:
  - Spec:    {spec}
  - Reviews:
{reviews}

Triage the reviews before planning:
  - Reviews may contradict each other. Read each one and decide which
    feedback to incorporate, which to reject, and why.
  - If the triage involves a real trade-off or a decision you cannot
    confidently make alone, ASK the operator — this is an interactive
    session.

When every trade-off is resolved, do TWO things in this order:

  1. UPDATE the spec file in place ({spec}) so it reflects the
     accepted review feedback and every decision you just made. The
     spec must end up representing the final, agreed-on design —
     another agent reading only the spec should not be surprised by
     anything in the plan.
  2. Write the plan to: {plan}

Hard rules — override anything the superpowers / writing-plans skill suggests:
  - Do NOT write or modify any code (source files, configs, build
    scripts). You may only edit the spec and write the plan.
  - Do NOT `git add`, `git commit`, `git stash`, or otherwise touch
    version control. The spec and plan stay untracked; a later phase
    commits. If the skill offers to commit, refuse.
  - The plan MUST be an execution map for coordination. It SHOULD include:
      - Sequencing and dependencies between work items (what order matters, and why)
      - Interfaces, integration points, and execution seams that must be honored
      - Constraints from the spec that narrow the correct solution space
      - Optional likely file/module touchpoints ONLY as orientation when helpful
  - The plan MUST NOT read like a pseudo-implementation or patch recipe:
      - No checkbox to-do lists or step-by-step coding instructions
      - No helper/function decomposition scripts or function-by-function edit sequences
      - No patch-like ordering of edits, "change this line then that line", or mini diffs
      - No mandated internal code shape (struct fields, method signatures, class layout)
        unless required by the spec or an explicit interface commitment needed for coordination
  - Authority rule:
      - The spec is the design contract and wins any conflict.
      - The plan is advisory for implementation shape.
      - The plan is authoritative ONLY for sequencing and explicit interface commitments
        it names for coordination. Do not turn advisory detail into an implementation contract.
  - Do NOT ask the operator whether to continue, proceed, start
    implementing, jump to coding, run the next skill, or skip any
    downstream stage. When the plan is written, STOP and exit — the
    orchestrator drives stage transitions, not you.
  - Do NOT offer to run tests, commit, or push anything.

The operator is here and ready to respond to clarifying questions
about the design itself.
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
        r#"You are splitting an approved plan into actionable, self-contained,
testable tasks. This is a NON-INTERACTIVE run — the operator is NOT
available. Do NOT modify any code in the repository; your ONLY output
is the tasks TOML file.

Inputs:
  - Spec: {spec}
  - Plan: {plan}

Read both carefully before sharding.

Rules:
  1. Decompose the plan into a sequence of tasks ONLY if the plan is
     large enough to warrant it. If the whole plan can reasonably be
     implemented by one coding session at ~200k tokens, a single-task
     tasks.toml is the correct answer — do NOT force artificial splits.
     Each task must be self-contained (buildable by one coding agent
     session without requiring another task to have shipped first,
     unless explicitly listed as a dependency in the task's description).
  2. Size each task at roughly 200_000 tokens of implementation effort —
     small enough that a coding agent can finish it in one session
     without context compaction, large enough to be meaningful. For a
     small plan this means exactly one task; for a bigger plan, split
     along natural seams (by subsystem, by layer, by phase).
  3. Each task MUST include:
       - id             sequential integer starting at 1
       - title          one-line summary
       - description    detailed what-to-do (multi-line TOML string allowed)
       - test           concrete verification steps (how will we know it's done)
       - estimated_tokens  your integer estimate (target ~200_000)
       - spec_refs      array of {{ path, lines }} pointing into the spec
       - plan_refs      array of {{ path, lines }} pointing into the plan
     The `lines` field is a range like "12-45" or a single number.

Hard rules — keep tasks outcome- and coordination-oriented:
  - Each task `description` SHOULD focus on required outcomes, dependencies /
    ordering constraints, acceptance checks, and relevant interfaces or
    touchpoints (including likely file/module touchpoints only as orientation).
  - Task descriptions MUST NOT be recipe-style coding scripts:
      - No step-by-step coding instructions ("do X, then Y, then Z")
      - No miniature edit scripts or pseudo-patch sequences
      - No mandated internal design or helper/function decomposition unless
        required by the spec or an explicit interface commitment needed for coordination
  - `plan_refs` MUST point to plan content about goals, sequencing,
    dependencies, or interface commitments. Do not point primarily to
    recipe-like implementation instructions.

Output: write the TOML to {tasks}
in EXACTLY this shape (double quotes for strings, triple quotes for
multi-line, arrays of inline tables for refs):

    [[tasks]]
    id = 1
    title = "Scaffold the worker pool"
    description = """
    Wire up a Tokio worker pool in src/pool.rs. …
    """
    test = """
    Run `cargo test pool::` — the new tests must pass.
    """
    estimated_tokens = 180000
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

The file will be validated programmatically — missing or empty fields
will cause rejection. Do not emit any prose around the TOML.
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        tasks = tasks_path.display(),
        instr = instr,
    )
}

fn coder_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    commit_file: &std::path::Path,
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
        r#"You are the coder for task {task_id}, round {round}. This is a
NON-INTERACTIVE run — the operator is NOT available during coding.
Make your own judgement calls, document them in the commit message,
and flag anything genuinely ambiguous in a line comment for the
reviewer to catch.

Task spec:      {task}
Spec (design):  {spec}
Plan:           {plan}
{prev_review}{resume_hint}
Your job:
  1. Read the task file first. It lists what to do, what to test, and line
     refs into the spec and plan for background.
  2. Implement the task end-to-end on the current branch.
  3. Make the tests described in the task pass.
  4. Commit your work with a clear message (see commit rules below).
  5. When finished, write the commit SHA to: {commit}
     (just the short SHA, one line). This is the signal for the TUI to
     pick up that your work is complete — the TUI polls for this file.

Commit message rules (MANDATORY — the reviewer WILL reject violations):
  - Use Conventional Commits: `type(scope): summary`, e.g.
    `feat(auth): add refresh-token rotation`, `fix(db): close pool on shutdown`.
    Common types: feat, fix, refactor, test, docs, chore, perf, style, build.
  - Do NOT add `Co-Authored-By:` trailers or any other co-author attribution.
  - Do NOT mention the orchestrator's internal vocabulary in the message:
    no "task <N>", no "round <N>", no "plan", no "shard", no "phase",
    no references to this prompt. Write the message as if a human engineer
    authored the change standalone.

Productivity rule — delegate tedious work to subagents:
  - For repetitive, multi-file, or exploration-heavy chores (bulk renames,
    codebase audits, test sweeps, dependency tracing, large refactors),
    dispatch a subagent. They run in parallel, stay focused, and finish
    faster than you doing it sequentially. Give each subagent a clear,
    self-contained brief and verify their output before committing.

Hard rules:
  - Do NOT ask clarifying questions; work from the task + spec + plan.
  - Stay within the scope of this one task. If you uncover follow-up work,
    do NOT do it yourself — note it for the reviewer instead.
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
        commit = commit_file.display(),
        instr = instr,
    )
}

fn reviewer_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    commit_file: &std::path::Path,
    review_file: &std::path::Path,
) -> String {
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let live_summary_path = session_dir.join("artifacts").join("live_summary.txt");
    let instr = live_summary_instruction(&live_summary_path);
    format!(
        r#"You are the reviewer for task {task_id}, round {round}. NON-INTERACTIVE —
the operator is NOT available. Do NOT modify code. Write ONLY the review TOML.

Inputs:
  Task:         {task}
  Spec:         {spec}
  Plan:         {plan}
  Commit SHA:   {commit}

Review the change carefully:
  1. `git show $(cat {commit})` to see what was actually committed.
  2. Verify the task's test description passes (run it, inspect code).
  3. Check for issues: correctness, missing edge cases, broken contracts,
     bad error handling, test gaps.

Emit the verdict to: {review}
in EXACTLY this TOML shape (double-quoted strings; triple-quoted for
multi-line; arrays of inline tables for any new task refs):

    status  = "done" | "revise" | "blocked"
    summary = "One-paragraph summary of what was done and your verdict."
    feedback = [
      "Specific thing to fix, if status is revise/blocked.",
      "One item per string.",
    ]

    # Optional: follow-up tasks to add to the queue when you find work
    # that is genuinely out-of-scope for this task but needed later.
    [[new_tasks]]
    id = 100
    title = "…"
    description = """…"""
    test = """…"""
    estimated_tokens = 150000
    spec_refs = [{{ path = "artifacts/spec.md", lines = "10-30" }}]
    plan_refs = [{{ path = "artifacts/plan.md", lines = "50-70" }}]

Rules:
  - status = "done"    → the task is complete and meets its tests.
  - status = "revise"  → the coder must iterate; feedback MUST list the
                          specific issues.
  - status = "blocked" → human judgement is required; feedback MUST explain
                          what's unclear or stuck.
  - Do NOT leave feedback empty for revise/blocked.
  - Do NOT emit prose outside the TOML.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        commit = commit_file.display(),
        review = review_file.display(),
        instr = instr,
    )
}
