mod events;
mod models;
mod render;
mod sections;
mod state;

use crate::{
    adapters::{AgentRun, adapter_for_vendor, launch_interactive, launch_noninteractive},
    cache,
    review,
    selection::{self, ModelStatus, QuotaError, select_for_review},
    state::{self as session_state, Phase, PhaseModel, SessionState},
    tasks,
    tmux::{self, TmuxContext},
    tui::AppTerminal,
};
use anyhow::Result;
use crossterm::event::{self, Event};

use self::{
    models::{spawn_refresh, vendor_tag},
    sections::{build_sections, current_section_index},
    state::{ModelRefreshState, PipelineSection},
};

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

const PREVIEW_LINES: usize = 3;

#[derive(Debug)]
pub struct App {
    tmux: TmuxContext,
    state: SessionState,
    sections: Vec<PipelineSection>,
    models: Vec<ModelStatus>,
    model_refresh: ModelRefreshState,
    selected: usize,
    expanded: BTreeSet<usize>,
    transcript_open: BTreeSet<usize>,
    section_scroll: Vec<usize>,
    body_inner_height: usize,
    input_mode: bool,
    input_buffer: String,
    confirm_back: bool,
    window_launched: bool,
    quota_errors: Vec<QuotaError>,
    quota_retry_delay: Duration,
    agent_line_count: usize,
    live_summary: String,
    live_summary_path: Option<std::path::PathBuf>,
    live_summary_mtime: Option<std::time::SystemTime>,
}

impl App {
    pub fn new(tmux: TmuxContext, state: SessionState) -> Self {
        let sections = build_sections(&state, false);
        let section_count = sections.len();
        let current = current_section_index(&sections);

        let (models, quota_errors, model_refresh) = match cache::load() {
            Some((cached, errors, expired)) => {
                let refresh = if expired {
                    ModelRefreshState::Fetching { rx: spawn_refresh(), started_at: Instant::now() }
                } else {
                    ModelRefreshState::Idle(Instant::now())
                };
                (cached, errors, refresh)
            }
            None => (Vec::new(), Vec::new(), ModelRefreshState::Fetching { rx: spawn_refresh(), started_at: Instant::now() }),
        };

        Self {
            tmux,
            state,
            sections,
            models,
            model_refresh,
            selected: current,
            expanded: BTreeSet::new(),
            transcript_open: BTreeSet::new(),
            section_scroll: vec![usize::MAX; section_count],
            body_inner_height: 0,
            input_mode: false,
            input_buffer: String::new(),
            confirm_back: false,
            window_launched: false,
            quota_errors,
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            live_summary: String::new(),
            live_summary_path: None,
            live_summary_mtime: None,
        }
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        loop {
            self.refresh_models_if_due();
            self.poll_agent_window();
            self.update_agent_progress();
            self.poll_live_summary();
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key) {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn current_section(&self) -> usize {
        current_section_index(&self.sections)
    }

    fn can_focus_input(&self) -> bool {
        self.is_expanded(self.selected) && self.sections[self.selected].input_placeholder.is_some()
    }

    fn is_expanded(&self, index: usize) -> bool {
        index == self.current_section() || self.expanded.contains(&index)
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
        let reserved = self.sections.len() + expanded_preview_count * PREVIEW_LINES;
        self.body_inner_height.saturating_sub(reserved).max(6)
    }

    fn section_scroll_offset(&self, index: usize, total: usize, limit: usize) -> usize {
        let max_offset = total.saturating_sub(limit);
        if self.section_scroll[index] == usize::MAX {
            max_offset
        } else {
            self.section_scroll[index].min(max_offset)
        }
    }

    fn editable_artifact(&self) -> Option<std::path::PathBuf> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let path = match self.state.current_phase {
            Phase::BrainstormRunning
            | Phase::SpecReviewRunning
            | Phase::SpecReviewPaused => artifacts.join("spec.md"),
            Phase::PlanningRunning => artifacts.join("plan.md"),
            Phase::ShardingRunning => artifacts.join("tasks.toml"),
            Phase::ImplementationRound(r) | Phase::ReviewRound(r) => {
                session_dir.join("rounds").join(format!("{r:03}")).join("task.md")
            }
            Phase::IdeaInput | Phase::Done | Phase::BlockedNeedsUser => return None,
        };
        if path.exists() { Some(path) } else { None }
    }

    fn open_editable_artifact(&self) {
        let Some(path) = self.editable_artifact() else { return };
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
                let _ = self.state.transition_to(Phase::IdeaInput);
            }
            Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                kill_window("[Spec Review]");
                for i in 1..=self.state.spec_reviewers.len().max(1) {
                    let _ = fs::remove_file(artifacts.join(format!("spec-review-{i}.md")));
                    let _ = fs::remove_file(prompts.join(format!("spec-review-{i}.md")));
                }
                self.state.spec_reviewers.clear();
                self.state.phase_models.remove("spec-review");
                let _ = self.state.transition_to(Phase::BrainstormRunning);
            }
            Phase::PlanningRunning => {
                kill_window("[Planning]");
                let _ = fs::remove_file(artifacts.join("plan.md"));
                let _ = self.state.transition_to(Phase::SpecReviewRunning);
            }
            Phase::ShardingRunning => {
                kill_window("[Sharding]");
                let _ = fs::remove_file(artifacts.join("tasks.toml"));
                let _ = fs::remove_file(prompts.join("sharding.md"));
                self.state.phase_models.remove("sharding");
                let _ = self.state.transition_to(Phase::PlanningRunning);
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
                let _ = self.state.transition_to(prev);
            }
            Phase::ReviewRound(r) => {
                kill_window(&format!("[Review r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let _ = self.state.transition_to(Phase::ImplementationRound(r));
            }
            Phase::IdeaInput | Phase::BlockedNeedsUser | Phase::Done => {}
        }

        self.state.agent_error = None;
        self.window_launched = false;
        let _ = self.state.save();
        self.sections = build_sections(&self.state, self.window_launched);
        self.section_scroll.resize(self.sections.len(), usize::MAX);
        self.selected = current_section_index(&self.sections);
    }

    fn record_attempt(&mut self, status: crate::state::AttemptStatus) {
        let section_idx = current_section_index(&self.sections);
        let section = &self.sections[section_idx];
        let key = phase_attempt_key(self.state.current_phase);

        let attempt = crate::state::PhaseAttempt {
            status,
            summary: section.summary.clone(),
            events: section.events.clone(),
            transcript: section.transcript.clone(),
            error: self.state.agent_error.clone(),
            live_summary: self.live_summary.clone(),
        };

        self.state.phase_attempts.entry(key).or_default().push(attempt);
    }

    fn poll_live_summary(&mut self) {
        if !self.window_launched {
            self.live_summary.clear();
            self.live_summary_mtime = None;
            return;
        }

        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary.clear();
            return;
        };

        let Ok(meta) = std::fs::metadata(&path) else {
            self.live_summary.clear();
            self.live_summary_mtime = None;
            return;
        };

        let Ok(mtime) = meta.modified() else {
            return;
        };

        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);

        if stale {
            self.live_summary.clear();
            return;
        }

        let should_read = match self.live_summary_mtime {
            None => true,
            Some(cached) => mtime > cached,
        };

        if should_read {
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.live_summary = content.trim().to_string();
                self.live_summary_mtime = Some(mtime);
            }
        }
    }

    fn update_agent_progress(&mut self) {
        if !self.window_launched {
            self.agent_line_count = 0;
            return;
        }
        let window_name_owned;
        let window_name: &str = match self.state.current_phase {
            Phase::BrainstormRunning => "[Brainstorm]",
            Phase::SpecReviewRunning => "[Spec Review]",
            Phase::PlanningRunning => "[Planning]",
            Phase::ShardingRunning => "[Sharding]",
            Phase::ImplementationRound(r) => {
                window_name_owned = format!("[Coder r{r}]");
                &window_name_owned
            }
            Phase::ReviewRound(r) => {
                window_name_owned = format!("[Review r{r}]");
                &window_name_owned
            }
            _ => return,
        };
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", window_name, "-p", "-J"])
            .output();
        if let Ok(out) = output {
            let text = String::from_utf8_lossy(&out.stdout);
            let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
            self.agent_line_count = lines;
        }
    }

    fn poll_agent_window(&mut self) {
        if !self.window_launched {
            return;
        }

        let session_dir = session_state::session_dir(&self.state.session_id);
        let coder_window: String;
        let reviewer_window: String;
        let (window_name, artifact_path, next_phase) = match self.state.current_phase {
            Phase::BrainstormRunning => (
                "[Brainstorm]",
                session_dir.join("artifacts").join("spec.md"),
                Phase::SpecReviewRunning,
            ),
            Phase::SpecReviewRunning => (
                "[Spec Review]",
                session_dir.join("artifacts")
                    .join(format!("spec-review-{}.md", self.state.spec_reviewers.len() + 1)),
                Phase::SpecReviewPaused,
            ),
            Phase::PlanningRunning => (
                "[Planning]",
                session_dir.join("artifacts").join("plan.md"),
                Phase::ShardingRunning,
            ),
            Phase::ShardingRunning => (
                "[Sharding]",
                session_dir.join("artifacts").join("tasks.toml"),
                Phase::ImplementationRound(1),
            ),
            Phase::ImplementationRound(r) => {
                let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
                coder_window = format!("[Coder r{r}]");
                (
                    coder_window.as_str(),
                    round_dir.join("commit.txt"),
                    Phase::ReviewRound(r),
                )
            }
            Phase::ReviewRound(r) => {
                let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
                reviewer_window = format!("[Review r{r}]");
                (
                    reviewer_window.as_str(),
                    round_dir.join("review.toml"),
                    Phase::ImplementationRound(r + 1),
                )
            }
            _ => return,
        };

        if tmux::window_exists(window_name) {
            return;
        }

        self.window_launched = false;

        // Snapshot attempt before any transition or rebuild.
        match self.state.current_phase {
            Phase::ImplementationRound(_) => {
                // Successful coder round defers recording until reviewer completes.
                if !artifact_path.exists() {
                    self.record_attempt(crate::state::AttemptStatus::Failed);
                }
            }
            _ => {
                let status = if artifact_path.exists() && self.state.agent_error.is_none() {
                    crate::state::AttemptStatus::Done
                } else {
                    crate::state::AttemptStatus::Failed
                };
                self.record_attempt(status);
            }
        }

        if artifact_path.exists() {
            if self.state.current_phase == Phase::ShardingRunning {
                match tasks::validate(&artifact_path) {
                    Ok(file) => {
                        self.state.agent_error = None;
                        let ids: Vec<u32> = file.tasks.iter().map(|t| t.id).collect();
                        self.state.builder = session_state::BuilderState {
                            pending: ids,
                            done: Vec::new(),
                            current_task: None,
                            iteration: 0,
                            coder_started: false,
                            reviewer_started: false,
                            last_verdict: None,
                        };
                    }
                    Err(e) => {
                        self.state.agent_error = Some(format!(
                            "tasks.toml invalid: {e} — retry or edit the file"
                        ));
                        let _ = self.state.save();
                        self.sections = build_sections(&self.state, self.window_launched);
                        self.section_scroll.resize(self.sections.len(), usize::MAX);
                        self.selected = current_section_index(&self.sections);
                        return;
                    }
                }
            } else {
                self.state.agent_error = None;
            }
            if self.state.current_phase == Phase::SpecReviewRunning {
                if let Some(pm) = self.state.phase_models.get("spec-review").cloned() {
                    self.state.spec_reviewers.push(pm);
                }
            }

            let resolved_next = if let Phase::ReviewRound(r) = self.state.current_phase {
                match review::validate(&artifact_path) {
                    Ok(v) => {
                        self.state.builder.last_verdict = Some(match v.status {
                            review::ReviewStatus::Done => "done",
                            review::ReviewStatus::Revise => "revise",
                            review::ReviewStatus::Blocked => "blocked",
                        }.to_string());
                        self.state.builder.reviewer_started = false;
                        match v.status {
                            review::ReviewStatus::Done => {
                                if let Some(id) = self.state.builder.current_task.take() {
                                    self.state.builder.done.push(id);
                                }
                                for t in v.new_tasks {
                                    self.state.builder.pending.push(t.id);
                                }
                                if self.state.builder.pending.is_empty() {
                                    Phase::Done
                                } else {
                                    self.state.builder.coder_started = false;
                                    Phase::ImplementationRound(r + 1)
                                }
                            }
                            review::ReviewStatus::Revise => {
                                self.state.builder.coder_started = true;
                                Phase::ImplementationRound(r + 1)
                            }
                            review::ReviewStatus::Blocked => Phase::BlockedNeedsUser,
                        }
                    }
                    Err(e) => {
                        self.state.agent_error = Some(format!("review TOML invalid: {e}"));
                        let _ = self.state.save();
                        self.sections = build_sections(&self.state, self.window_launched);
                        self.section_scroll.resize(self.sections.len(), usize::MAX);
                        self.selected = current_section_index(&self.sections);
                        return;
                    }
                }
            } else if matches!(self.state.current_phase, Phase::ImplementationRound(_)) {
                self.state.builder.coder_started = false;
                next_phase
            } else {
                next_phase
            };

            let _ = self.state.transition_to(resolved_next);
        } else {
            let error = format!(
                "agent window closed without producing {}",
                artifact_path.display()
            );
            self.state.agent_error = Some(error);
            let _ = self.state.save();
        }

        self.sections = build_sections(&self.state, self.window_launched);
        self.section_scroll.resize(self.sections.len(), usize::MAX);
        self.selected = current_section_index(&self.sections);
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
            .join("rounds").join(format!("{round:03}"));
        let _ = std::fs::create_dir_all(&round_dir);
        self.state.builder.current_task
    }

    fn launch_brainstorm(&mut self, idea: String) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        }

        let Some(chosen) = selection::select(&self.models, selection::TaskKind::Build)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota — check model strip".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        };
        let (model, vendor_kind, vendor) = chosen;

        let session_id = &self.state.session_id;
        let prompt_path = session_state::session_dir(session_id).join("prompts").join("brainstorm.md");
        let spec_path = session_state::session_dir(session_id).join("artifacts").join("spec.md");

        let _ = std::fs::remove_file(&spec_path);

        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let live_summary_path = session_state::session_dir(session_id).join("artifacts").join("live_summary.txt");
        let prompt = brainstorm_prompt(&idea, &spec_path.display().to_string(), &live_summary_path.display().to_string());
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected]
                .events
                .push(format!("error writing prompt: {e}"));
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
                self.state.phase_models.insert(
                    "brainstorm".to_string(),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.transition_to(Phase::BrainstormRunning);
                self.window_launched = true;
                self.live_summary_path = Some(session_state::session_dir(&self.state.session_id).join("artifacts").join("live_summary.txt"));
                self.live_summary.clear();
                self.live_summary_mtime = None;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected]
                    .events
                    .push(format!("failed to launch brainstorm: {e}"));
            }
        }
    }

    fn launch_spec_review(&mut self) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        }

        let session_id = self.state.session_id.clone();
        let spec_path = session_state::session_dir(&session_id).join("artifacts").join("spec.md");
        let review_n = self.state.spec_reviewers.len() + 1;
        let review_path = session_state::session_dir(&session_id).join("artifacts")
            .join(format!("spec-review-{review_n}.md"));

        let mut excluded = self.state.spec_reviewers.clone();
        if let Some(pm) = self.state.phase_models.get("brainstorm").cloned() {
            excluded.push(pm);
        }

        let chosen = select_for_review(&self.models, &excluded)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()));

        let (model, vendor_kind, vendor) = match chosen {
            Some(c) => c,
            None => {
                self.state.agent_error = Some("no unused model available for review".to_string());
                let _ = self.state.save();
                self.sections = build_sections(&self.state, self.window_launched);
                return;
            }
        };

        let _ = std::fs::remove_file(&review_path);

        let prompt_path = session_state::session_dir(&session_id).join("prompts")
            .join(format!("spec-review-{review_n}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let live_summary_path = session_state::session_dir(&session_id).join("artifacts").join("live_summary.txt");
        let prompt = spec_review_prompt(&spec_path.display().to_string(), &review_path.display().to_string(), &live_summary_path.display().to_string());
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive("[Spec Review]", &run, adapter.as_ref()) {
            Ok(()) => {
                self.state.phase_models.insert(
                    "spec-review".to_string(),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.save();
                self.window_launched = true;
                self.live_summary_path = Some(session_state::session_dir(&self.state.session_id).join("artifacts").join("live_summary.txt"));
                self.live_summary.clear();
                self.live_summary_mtime = None;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected].events.push(format!("failed to launch spec review: {e}"));
            }
        }
    }

    fn launch_planning(&mut self) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        }

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");

        let review_paths: Vec<std::path::PathBuf> = (1..=self.state.spec_reviewers.len())
            .map(|i| session_dir.join("artifacts").join(format!("spec-review-{i}.md")))
            .filter(|p| p.exists())
            .collect();

        let Some(chosen) = selection::select(&self.models, selection::TaskKind::Planning)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
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
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_interactive("[Planning]", &run, adapter.as_ref(), true) {
            Ok(()) => {
                self.state.phase_models.insert(
                    "planning".to_string(),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.save();
                self.window_launched = true;
                self.live_summary_path = Some(session_state::session_dir(&self.state.session_id).join("artifacts").join("live_summary.txt"));
                self.live_summary.clear();
                self.live_summary_mtime = None;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected].events.push(format!("failed to launch planning: {e}"));
            }
        }
    }

    fn launch_sharding(&mut self) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
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
            self.sections = build_sections(&self.state, self.window_launched);
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
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive("[Sharding]", &run, adapter.as_ref()) {
            Ok(()) => {
                self.state.phase_models.insert(
                    "sharding".to_string(),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.save();
                self.window_launched = true;
                self.live_summary_path = Some(session_state::session_dir(&self.state.session_id).join("artifacts").join("live_summary.txt"));
                self.live_summary.clear();
                self.live_summary_mtime = None;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected].events.push(format!("failed to launch sharding: {e}"));
            }
        }
    }

    fn launch_coder(&mut self) {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        }
        let Phase::ImplementationRound(r) = self.state.current_phase else { return };

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
        let resume = self.state.builder.coder_started;
        let prompt = coder_prompt(&session_dir, task_id, r, &task_file, &commit_file, resume);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive(&format!("[Coder r{r}]"), &run, adapter.as_ref()) {
            Ok(()) => {
                self.state.phase_models.insert(
                    format!("coder-r{r}"),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                self.state.builder.coder_started = true;
                let _ = self.state.save();
                self.window_launched = true;
                self.live_summary_path = Some(session_state::session_dir(&self.state.session_id).join("artifacts").join("live_summary.txt"));
                self.live_summary.clear();
                self.live_summary_mtime = None;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected].events.push(format!("failed to launch coder: {e}"));
            }
        }
    }

    fn launch_reviewer(&mut self) {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        }
        let Phase::ReviewRound(r) = self.state.current_phase else { return };
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

        let coder_pm = self.state.phase_models.get(&format!("coder-r{r}")).cloned();
        let excluded = coder_pm.into_iter().collect::<Vec<_>>();
        let Some(chosen) = select_for_review(&self.models, &excluded)
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt_path = session_dir.join("prompts").join(format!("reviewer-r{r}.md"));
        let prompt = reviewer_prompt(&session_dir, task_id, r, &task_file, &commit_file, &review_path);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive(&format!("[Review r{r}]"), &run, adapter.as_ref()) {
            Ok(()) => {
                self.state.phase_models.insert(
                    format!("reviewer-r{r}"),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                self.state.builder.reviewer_started = true;
                let _ = self.state.save();
                self.window_launched = true;
                self.live_summary_path = Some(session_state::session_dir(&self.state.session_id).join("artifacts").join("live_summary.txt"));
                self.live_summary.clear();
                self.live_summary_mtime = None;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected].events.push(format!("failed to launch reviewer: {e}"));
            }
        }
    }
}

fn phase_attempt_key(phase: Phase) -> String {
    match phase {
        Phase::BrainstormRunning => "brainstorm".to_string(),
        Phase::SpecReviewRunning => "spec-review".to_string(),
        Phase::PlanningRunning => "planning".to_string(),
        Phase::ShardingRunning => "sharding".to_string(),
        Phase::ImplementationRound(r) | Phase::ReviewRound(r) => format!("builder-round-{r}"),
        _ => "unknown".to_string(),
    }
}

fn kill_window(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-window", "-t", name])
        .output();
}

fn task_body_for(session_dir: &std::path::Path, task_id: u32) -> anyhow::Result<String> {
    use anyhow::Context;
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml")?;
    let task = parsed.tasks.iter().find(|t| t.id == task_id)
        .ok_or_else(|| anyhow::anyhow!("task id {task_id} not found"))?;
    let refs = |rs: &[crate::tasks::Ref]| -> String {
        if rs.is_empty() {
            "(none)".to_string()
        } else {
            rs.iter().map(|r| format!("  - {} lines {}", r.path, r.lines))
                .collect::<Vec<_>>().join("\n")
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

The operator is here and ready to respond to your questions.
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

Hard rules:
  - Do NOT write or modify any code (source files, configs, build
    scripts). You may only edit the spec and write the plan.
  - Do NOT ask the operator whether to start implementing, whether to
    jump straight to coding, or whether to skip any downstream phase.
    Implementation is a separate later phase handled by a different
    agent — your job ends once the plan is written.
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
        let p = session_dir.join("rounds").join(format!("{:03}", round - 1)).join("review.toml");
        if p.exists() {
            format!("\nPrevious reviewer feedback (round {}): {}\nRead it first and address every feedback item.\n", round - 1, p.display())
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
  4. Commit your work with a clear message.
  5. When finished, write the commit SHA to: {commit}
     (just the short SHA, one line). This is the signal that the round is
     complete — the TUI polls for this file.

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
