use crate::{
    adapters::{AgentRun, adapter_for_vendor, launch_interactive, launch_noninteractive},
    cache,
    selection::{self, ModelStatus, QuotaError, VendorKind, select_for_review},
    state::{self, Phase, PhaseModel, RunState},
    tmux::{self, TmuxContext},
    tui::AppTerminal,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::{
    collections::BTreeSet,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const PREVIEW_LINES: usize = 3;

#[derive(Debug)]
pub struct App {
    tmux: TmuxContext,
    state: RunState,
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
    /// Number of non-empty lines in the agent window; used to drive a
    /// looping progress spinner that advances as the agent streams output.
    agent_line_count: usize,
}

#[derive(Debug)]
struct PipelineSection {
    name: String,
    status: SectionStatus,
    summary: String,
    events: Vec<String>,
    transcript: Vec<String>,
    input_placeholder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionStatus {
    Pending,
    Running,
    WaitingUser,
    Done,
}

#[derive(Debug)]
enum ModelRefreshState {
    Fetching {
        rx: mpsc::Receiver<(Vec<ModelStatus>, Vec<QuotaError>)>,
        started_at: Instant,
    },
    Idle(Instant),
}

impl App {
    pub fn new(tmux: TmuxContext, state: RunState) -> Self {
        let sections = build_sections(&state, false);
        let section_count = sections.len();
        let current = current_section_index(&sections);

        // Load cached models immediately so the UI is populated on first frame
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
        }
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        loop {
            self.refresh_models_if_due();
            self.poll_agent_window();
            self.update_agent_progress();
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

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let model_height = (self.models.len() + self.quota_errors.len()).max(1) as u16 + 2;
        let root = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(8),
                Constraint::Length(model_height),
            ])
            .split(frame.area());

        self.body_inner_height = root[1].height.saturating_sub(2) as usize;
        self.clamp_scroll();

        frame.render_widget(self.header(), root[0]);
        frame.render_widget(self.pipeline_view(), root[1]);
        frame.render_widget(self.model_strip(), root[2]);
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        if self.input_mode {
            return self.handle_input_key(key);
        }

        // Any key other than 'b' cancels a pending back-confirmation
        if self.confirm_back && key.code != KeyCode::Char('b') {
            self.confirm_back = false;
            return false;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('b') => {
                if self.confirm_back {
                    self.confirm_back = false;
                    self.go_back();
                } else if self.can_go_back() {
                    self.confirm_back = true;
                }
                false
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                false
            }
            KeyCode::Down => {
                if self.selected + 1 < self.sections.len() {
                    self.selected += 1;
                }
                false
            }
            KeyCode::Enter => {
                let on_current = self.selected == self.current_section();
                // Phases that just need Enter pressed (no text input) —
                // only fire when the user is focused on the active phase section
                if on_current {
                    if self.state.current_phase == Phase::SpecReviewPaused {
                        // Add another review round
                        let _ = self.state.transition_to(Phase::SpecReviewRunning);
                        self.launch_spec_review();
                        return false;
                    }
                    if self.state.current_phase == Phase::BrainstormRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        let idea = self.state.idea_text.clone().unwrap_or_default();
                        self.launch_brainstorm(idea);
                        return false;
                    }
                    if self.state.current_phase == Phase::SpecReviewRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_spec_review();
                        return false;
                    }
                    if self.state.current_phase == Phase::PlanningRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_planning();
                        return false;
                    }
                }
                if self.can_focus_input() {
                    self.input_mode = true;
                } else {
                    self.toggle_selected_section();
                }
                false
            }
            KeyCode::Char('n') => {
                // Proceed to planning from either:
                //   - SpecReviewPaused (reviews done, user chooses to move on)
                //   - SpecReviewRunning with error (skip a failed review)
                let can_skip = self.state.current_phase == Phase::SpecReviewPaused
                    || (self.state.current_phase == Phase::SpecReviewRunning
                        && self.state.agent_error.is_some());
                if can_skip {
                    self.state.agent_error = None;
                    let _ = self.state.transition_to(Phase::PlanningRunning);
                    self.sections = build_sections(&self.state, self.window_launched);
                    self.section_scroll.resize(self.sections.len(), usize::MAX);
                    self.selected = self.sections.iter()
                        .position(|s| s.name == "Planning")
                        .unwrap_or_else(|| current_section_index(&self.sections));
                }
                false
            }
            KeyCode::Char('e') => {
                self.open_editable_artifact();
                false
            }
            KeyCode::Char('t') => {
                self.toggle_transcript();
                false
            }
            KeyCode::PageUp => {
                self.scroll_selected(-(self.page_step() as isize));
                false
            }
            KeyCode::PageDown => {
                self.scroll_selected(self.page_step() as isize);
                false
            }
            _ => false,
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = false;
                false
            }
            KeyCode::Enter => {
                let trimmed = self.input_buffer.trim().to_string();
                if !trimmed.is_empty() {
                    if trimmed == "/exit" {
                        return true;
                    }

                    if trimmed == "/stats" || trimmed == "/status" || trimmed == "/usage" {
                        self.force_refresh_models();
                        self.sections[self.selected]
                            .transcript
                            .push(format!("> {trimmed}"));
                        self.sections[self.selected]
                            .transcript
                            .push("< refreshing model quotas...".to_string());
                        self.input_buffer.clear();
                        self.input_mode = false;
                        return false;
                    }

                    // IdeaInput phase: submit idea and launch brainstorm
                    if self.state.current_phase == Phase::IdeaInput {
                        self.input_buffer.clear();
                        self.input_mode = false;
                        self.launch_brainstorm(trimmed);
                        return false;
                    }

                    self.sections[self.selected]
                        .transcript
                        .push(format!("> {trimmed}"));
                    self.input_buffer.clear();
                }
                self.input_mode = false;
                false
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                false
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input_buffer.push(c);
                false
            }
            _ => false,
        }
    }

    fn force_refresh_models(&mut self) {
        self.model_refresh = ModelRefreshState::Fetching { rx: spawn_refresh(), started_at: Instant::now() };
    }

    fn editable_artifact(&self) -> Option<std::path::PathBuf> {
        let run_dir = state::run_dir(&self.state.run_id);
        let artifacts = run_dir.join("artifacts");
        let path = match self.state.current_phase {
            Phase::BrainstormRunning
            | Phase::SpecReviewRunning
            | Phase::SpecReviewPaused => artifacts.join("spec.md"),
            Phase::PlanningRunning
            | Phase::PlanReviewRunning
            | Phase::AwaitingPlanApproval => artifacts.join("plan.md"),
            Phase::ImplementationRound(r) | Phase::ReviewRound(r) => {
                run_dir.join("rounds").join(format!("{r:03}")).join("task.md")
            }
            Phase::IdeaInput | Phase::Done | Phase::BlockedNeedsUser => return None,
        };
        if path.exists() { Some(path) } else { None }
    }

    fn open_editable_artifact(&self) {
        let Some(path) = self.editable_artifact() else { return };
        let path_str = path.display().to_string();
        // Open in a new tmux window; window closes when vim exits
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

        let run_dir = state::run_dir(&self.state.run_id);
        let artifacts = run_dir.join("artifacts");
        let prompts = run_dir.join("prompts");

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
                // Remove all spec review artifacts
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
            Phase::PlanReviewRunning => {
                kill_window("[Plan Review 1]");
                kill_window("[Plan Review 2]");
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(artifacts.join("plan-review-2.md"));
                let _ = self.state.transition_to(Phase::PlanningRunning);
            }
            Phase::AwaitingPlanApproval => {
                let _ = self.state.transition_to(Phase::PlanReviewRunning);
            }
            Phase::ImplementationRound(r) => {
                kill_window(&format!("[Coder r{r}]"));
                let _ = fs::remove_dir_all(run_dir.join("rounds").join(format!("{r:03}")));
                let prev = if r <= 1 {
                    Phase::AwaitingPlanApproval
                } else {
                    Phase::ReviewRound(r - 1)
                };
                let _ = self.state.transition_to(prev);
            }
            Phase::ReviewRound(r) => {
                kill_window(&format!("[Review r{r}]"));
                let _ = fs::remove_dir_all(run_dir.join("rounds").join(format!("{r:03}")));
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

    fn launch_spec_review(&mut self) {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error = Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.sections = build_sections(&self.state, self.window_launched);
            return;
        }

        let run_id = self.state.run_id.clone();
        let spec_path = state::run_dir(&run_id).join("artifacts").join("spec.md");
        let review_n = self.state.spec_reviewers.len() + 1;
        let review_path = state::run_dir(&run_id).join("artifacts")
            .join(format!("spec-review-{review_n}.md"));

        // Build exclusion list: brainstorm model + all previous reviewers
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

        // Delete any leftover artifact so poll only advances when this run produces it
        let _ = std::fs::remove_file(&review_path);

        let prompt_path = state::run_dir(&run_id).join("prompts")
            .join(format!("spec-review-{review_n}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let prompt = spec_review_prompt(&spec_path.display().to_string(), &review_path.display().to_string());
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            run_id: run_id.clone(),
            phase: "spec-review".to_string(),
            role: "spec-review".to_string(),
            model: model.clone(),
            prompt_path: prompt_path.clone(),
            artifact_paths: vec![review_path.clone()],
        };

        let adapter = adapter_for_vendor(vendor_kind);
        match launch_noninteractive("[Spec Review]", &run, adapter.as_ref()) {
            Ok(()) => {
                // Record model now so the running section can show it;
                // phase transition only happens in poll_agent_window after
                // the artifact is verified to exist.
                self.state.phase_models.insert(
                    "spec-review".to_string(),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.save();
                self.window_launched = true;
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

        let run_id = self.state.run_id.clone();
        let run_dir = state::run_dir(&run_id);
        let spec_path = run_dir.join("artifacts").join("spec.md");
        let plan_path = run_dir.join("artifacts").join("plan.md");

        // Collect all spec review artifact paths
        let review_paths: Vec<std::path::PathBuf> = (1..=self.state.spec_reviewers.len())
            .map(|i| run_dir.join("artifacts").join(format!("spec-review-{i}.md")))
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

        let prompt_path = run_dir.join("prompts").join("planning.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let prompt = planning_prompt(&spec_path, &review_paths, &plan_path);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected].events.push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            run_id: run_id.clone(),
            phase: "planning".to_string(),
            role: "planning".to_string(),
            model: model.clone(),
            prompt_path: prompt_path.clone(),
            artifact_paths: vec![plan_path.clone()],
        };

        let adapter = adapter_for_vendor(vendor_kind);
        // Planning is interactive — switch to the window so the user can engage
        match launch_interactive("[Planning]", &run, adapter.as_ref(), true) {
            Ok(()) => {
                self.state.phase_models.insert(
                    "planning".to_string(),
                    PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.save();
                self.window_launched = true;
                self.sections = build_sections(&self.state, self.window_launched);
                self.section_scroll.resize(self.sections.len(), usize::MAX);
                self.selected = current_section_index(&self.sections);
            }
            Err(e) => {
                self.sections[self.selected].events.push(format!("failed to launch planning: {e}"));
            }
        }
    }

    /// Count non-empty lines in the agent's tmux window to drive the progress
    /// spinner. Each new line → spinner advances one step.
    fn update_agent_progress(&mut self) {
        if !self.window_launched {
            self.agent_line_count = 0;
            return;
        }
        let window_name = match self.state.current_phase {
            Phase::BrainstormRunning => "[Brainstorm]",
            Phase::SpecReviewRunning => "[Spec Review]",
            Phase::PlanningRunning => "[Planning]",
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

        let run_dir = state::run_dir(&self.state.run_id);
        let (window_name, artifact_path, next_phase) = match self.state.current_phase {
            Phase::BrainstormRunning => (
                "[Brainstorm]",
                run_dir.join("artifacts").join("spec.md"),
                Phase::SpecReviewRunning,
            ),
            Phase::SpecReviewRunning => (
                "[Spec Review]",
                run_dir.join("artifacts")
                    .join(format!("spec-review-{}.md", self.state.spec_reviewers.len() + 1)),
                Phase::SpecReviewPaused,
            ),
            Phase::PlanningRunning => (
                "[Planning]",
                run_dir.join("artifacts").join("plan.md"),
                Phase::AwaitingPlanApproval,
            ),
            _ => return,
        };

        // Window is still alive — nothing to do yet
        if tmux::window_exists(window_name) {
            return;
        }

        // Window is gone — check if the required artifact was produced
        self.window_launched = false;
        if artifact_path.exists() {
            self.state.agent_error = None;
            // Record the reviewer before transitioning
            if self.state.current_phase == Phase::SpecReviewRunning {
                if let Some(pm) = self.state.phase_models.get("spec-review").cloned() {
                    self.state.spec_reviewers.push(pm);
                }
            }
            let _ = self.state.transition_to(next_phase);
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
        // Keep the cursor on the active phase section so the user sees its new state
        self.selected = current_section_index(&self.sections);
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

        let run_id = &self.state.run_id;
        let prompt_path = state::run_dir(run_id).join("prompts").join("brainstorm.md");
        let spec_path = state::run_dir(run_id).join("artifacts").join("spec.md");

        // Delete any leftover artifact so poll_agent_window only advances
        // when this run's agent actually produces the file.
        let _ = std::fs::remove_file(&spec_path);

        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let prompt = brainstorm_prompt(&idea, &spec_path.display().to_string());
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.sections[self.selected]
                .events
                .push(format!("error writing prompt: {e}"));
            return;
        }

        let run = AgentRun {
            run_id: run_id.clone(),
            phase: "brainstorm".to_string(),
            role: "brainstorm".to_string(),
            model: model.clone(),
            prompt_path: prompt_path.clone(),
            artifact_paths: vec![spec_path.clone()],
        };

        let adapter = adapter_for_vendor(vendor_kind);
        let fresh_start = self.state.current_phase == Phase::IdeaInput;
        match launch_interactive("[Brainstorm]", &run, adapter.as_ref(), fresh_start) {
            Ok(()) => {
                self.state.idea_text = Some(idea.clone());
                self.state.selected_model = Some(model.clone());
                self.state.phase_models.insert(
                    "brainstorm".to_string(),
                    state::PhaseModel { model: model.clone(), vendor: vendor.clone() },
                );
                let _ = self.state.transition_to(Phase::BrainstormRunning);
                self.window_launched = true;
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

    fn header(&self) -> Paragraph<'_> {
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Codexize",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" #{} ", self.state.run_id)),
            Span::styled(
                format!("[{}]", self.state.current_phase.label()),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" | "),
            Span::raw(format!(
                "{}:{} {}",
                self.tmux.session_name, self.tmux.window_index, self.tmux.window_name
            )),
            Span::styled(
                {
                    let e = if self.editable_artifact().is_some() { " e" } else { "" };
                    let show_n = self.state.current_phase == Phase::SpecReviewPaused
                        || (self.state.current_phase == Phase::SpecReviewRunning
                            && self.state.agent_error.is_some());
                    let n = if show_n { " n" } else { "" };
                    format!(" | Up/Down Enter t PgUp/PgDn b{e}{n} q")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]))
    }

    fn pipeline_view(&self) -> Paragraph<'static> {
        let mut lines = Vec::new();
        let current = self.current_section();
        let selected_limit = self.selected_body_limit();
        let mut selected_header_line = 0usize;

        for (index, section) in self.sections.iter().enumerate() {
            let expanded = self.is_expanded(index);
            if index == self.selected {
                selected_header_line = lines.len();
            }

            lines.push(self.section_header(index, expanded, section));

            if expanded {
                let body_lines = self.section_body(index);
                if index == self.selected {
                    let visible = self.visible_selected_body(&body_lines, selected_limit, index);
                    lines.extend(visible);
                } else {
                    lines.extend(self.preview_body(&body_lines));
                }
            } else if index > current && section.status == SectionStatus::Pending {
                // keep pending future phases terse
            }
        }

        let viewport = self.body_inner_height;
        let max_scroll = lines.len().saturating_sub(viewport);
        let scroll = selected_header_line.saturating_sub(1).min(max_scroll) as u16;

        Paragraph::new(lines)
            .block(Block::default().title("Pipeline").borders(Borders::ALL))
            .scroll((scroll, 0))
    }

    fn model_strip(&self) -> Paragraph<'static> {
        // Group models by vendor, preserving existing order within each vendor
        let mut vendor_order: Vec<VendorKind> = Vec::new();
        let mut by_vendor: std::collections::BTreeMap<VendorKind, Vec<&ModelStatus>> =
            std::collections::BTreeMap::new();
        for model in &self.models {
            if !vendor_order.contains(&model.vendor) {
                vendor_order.push(model.vendor);
            }
            by_vendor.entry(model.vendor).or_default().push(model);
        }

        let mut lines: Vec<Line<'static>> = Vec::new();
        for vendor in &vendor_order {
            let tag = vendor_tag(*vendor);
            let tag_color = vendor_color(*vendor);
            let prefix = vendor_prefix(*vendor);
            let models = &by_vendor[vendor];

            for (i, model) in models.iter().enumerate() {
                let short_name = model
                    .name
                    .strip_prefix(prefix)
                    .unwrap_or(&model.name)
                    .to_string();

                let stupid_level = model
                    .stupid_level
                    .map(|v| format!("{v:>2}"))
                    .unwrap_or_else(|| " -".to_string());
                let quota = model
                    .quota_percent
                    .map(|v| format!("{v:>3}%"))
                    .unwrap_or_else(|| " --%".to_string());

                // Vendor tag only on first row of each group; blank pad on rest
                let tag_span = if i == 0 {
                    Span::styled(
                        format!("{:<8}", tag),
                        Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("        ")
                };

                lines.push(Line::from(vec![
                    tag_span,
                    Span::styled(
                        format!("{:<28}", short_name),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(stupid_level, Style::default().fg(Color::Yellow)),
                    Span::raw("  "),
                    Span::styled(quota, Style::default().fg(Color::Green)),
                    Span::raw("  "),
                    Span::styled(
                        format!(
                            "I:{:>2} P:{:>2} B:{:>2} R:{:>2}",
                            model.idea_rank,
                            model.planning_rank,
                            model.build_rank,
                            model.review_rank
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }

        // Quota fetch error warnings (only shown when present)
        for err in &self.quota_errors {
            let tag = vendor_tag(err.vendor);
            // Truncate error message to keep it on one line
            let msg = if err.message.len() > 60 {
                format!("{}...", &err.message[..60])
            } else {
                err.message.clone()
            };
            // Compute next retry time
            let retry_in = match &self.model_refresh {
                ModelRefreshState::Idle(at) => {
                    let elapsed = at.elapsed();
                    let due = self.quota_retry_delay;
                    if elapsed < due {
                        let secs = (due - elapsed).as_secs();
                        format!(" — retry in {}m{}s", secs / 60, secs % 60)
                    } else {
                        " — retrying...".to_string()
                    }
                }
                ModelRefreshState::Fetching { .. } => " — retrying now".to_string(),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  ⚠ {:<6}  ", tag),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{msg}{retry_in}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        Paragraph::new(lines).block(Block::default().title("Models").borders(Borders::ALL))
    }

    fn section_header(
        &self,
        index: usize,
        expanded: bool,
        section: &PipelineSection,
    ) -> Line<'static> {
        let marker = if expanded { "v" } else { ">" };
        let is_current = index == current_section_index(&self.sections);
        let style = if index == self.selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::raw(format!("{marker} ")),
            Span::raw(section.name.clone()),
            Span::raw(" | "),
            Span::styled(section.status.label(), section.status.style()),
            Span::raw(" | "),
            Span::styled(section.summary.clone(), Style::default().fg(Color::Gray)),
        ];

        if self.confirm_back && is_current {
            spans.push(Span::styled(
                "  [b again to go back and clean up — any other key to cancel]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }

        Line::from(spans).style(style)
    }

    fn section_body(&self, index: usize) -> Vec<Line<'static>> {
        let section = &self.sections[index];
        let mut lines = section
            .events
            .iter()
            .map(|event| {
                Line::from(vec![
                    Span::styled("  - ", Style::default().fg(Color::DarkGray)),
                    Span::raw(event.clone()),
                ])
            })
            .collect::<Vec<_>>();

        // Live progress spinner + model for running phases
        if section.status == SectionStatus::Running && self.window_launched {
            let phase_key = match self.state.current_phase {
                Phase::BrainstormRunning => Some("brainstorm"),
                Phase::SpecReviewRunning => Some("spec-review"),
                _ => None,
            };
            if let Some(key) = phase_key {
                let model_label = self.state.phase_models.get(key)
                    .map(|pm| format!("{} ({})", pm.model, pm.vendor))
                    .unwrap_or_else(|| "unknown model".to_string());
                let spin = spinner_frame(self.agent_line_count);
                lines.insert(0, Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(spin, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::styled(model_label, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!(" · {} lines", self.agent_line_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }

        if section.events.is_empty() {
            lines.push(Line::from(Span::styled(
                "  - no events yet",
                Style::default().fg(Color::DarkGray),
            )));
        }

        if !section.transcript.is_empty() {
            if self.transcript_open.contains(&index) {
                lines.push(Line::from(Span::styled(
                    "  transcript",
                    Style::default().fg(Color::Magenta),
                )));
                lines.extend(section.transcript.iter().map(|line| {
                    Line::from(vec![
                        Span::styled("    ", Style::default().fg(Color::DarkGray)),
                        Span::raw(line.clone()),
                    ])
                }));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("  [t] transcript hidden ({})", section.transcript.len()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        if let Some(placeholder) = &section.input_placeholder {
            let active = self.input_mode && index == self.selected;
            let frame_color = if active { Color::Yellow } else { Color::DarkGray };
            let width = 64usize;

            lines.push(Line::from(""));

            // Top border with label
            let label = if active { " typing " } else { " input " };
            let fill = width.saturating_sub(label.len() + 2);
            let top = format!(
                "  ╭{label}{}╮",
                "─".repeat(fill),
            );
            lines.push(Line::from(Span::styled(top, Style::default().fg(frame_color))));

            // Content row
            let (text, text_style) = if self.input_buffer.is_empty() {
                (placeholder.clone(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))
            } else {
                (self.input_buffer.clone(), Style::default().fg(Color::White))
            };
            let cursor = if active { "▌" } else { "" };
            let content_visible_len = text.chars().count() + cursor.chars().count();
            let inner_width = width.saturating_sub(2); // minus the two ╴ frame chars
            let padding = inner_width.saturating_sub(content_visible_len);
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(frame_color)),
                Span::styled(text, text_style),
                Span::styled(
                    cursor.to_string(),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::raw(" ".repeat(padding.saturating_sub(2))),
                Span::styled(" │", Style::default().fg(frame_color)),
            ]));

            // Bottom border with hint
            let hint = if active { " Enter: submit · Esc: cancel " } else { " Enter to type " };
            let fill = width.saturating_sub(hint.len() + 2);
            let bottom = format!("  ╰{}{hint}╯", "─".repeat(fill));
            lines.push(Line::from(Span::styled(bottom, Style::default().fg(frame_color))));
        }

        lines
    }

    fn visible_selected_body(
        &self,
        body_lines: &[Line<'static>],
        limit: usize,
        index: usize,
    ) -> Vec<Line<'static>> {
        if body_lines.is_empty() {
            return Vec::new();
        }

        let max_offset = body_lines.len().saturating_sub(limit);
        let offset = self.section_scroll_offset(index, body_lines.len(), limit);
        let end = (offset + limit).min(body_lines.len());
        let mut visible = Vec::new();

        if offset > 0 {
            visible.push(Line::from(Span::styled(
                "  ... older ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        visible.extend(body_lines[offset..end].iter().cloned());

        if offset < max_offset {
            visible.push(Line::from(Span::styled(
                "  ... newer ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        visible
    }

    fn preview_body(&self, body_lines: &[Line<'static>]) -> Vec<Line<'static>> {
        if body_lines.is_empty() {
            return Vec::new();
        }

        let start = body_lines.len().saturating_sub(PREVIEW_LINES);
        let mut visible = Vec::new();

        if start > 0 {
            visible.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        visible.extend(body_lines[start..].iter().cloned());
        visible
    }

    fn toggle_selected_section(&mut self) {
        let current = self.current_section();
        if self.selected == current {
            return;
        }

        if self.sections[self.selected].status == SectionStatus::Pending {
            return;
        }

        if !self.expanded.insert(self.selected) {
            self.expanded.remove(&self.selected);
            self.transcript_open.remove(&self.selected);
        }
    }

    fn toggle_transcript(&mut self) {
        if !self.is_expanded(self.selected) || self.sections[self.selected].transcript.is_empty() {
            return;
        }

        if !self.transcript_open.insert(self.selected) {
            self.transcript_open.remove(&self.selected);
        }
    }

    fn scroll_selected(&mut self, delta: isize) {
        if !self.is_expanded(self.selected) {
            return;
        }

        let limit = self.selected_body_limit();
        let total = self.section_body(self.selected).len();
        let max_offset = total.saturating_sub(limit) as isize;
        let current = self.section_scroll_offset(self.selected, total, limit) as isize;
        let next = (current + delta).clamp(0, max_offset);
        self.section_scroll[self.selected] = next as usize;
    }

    fn clamp_scroll(&mut self) {
        let limit = self.selected_body_limit();
        let total = self.section_body(self.selected).len();
        let max_offset = total.saturating_sub(limit);

        if self.section_scroll[self.selected] != usize::MAX {
            self.section_scroll[self.selected] = self.section_scroll[self.selected].min(max_offset);
        }
    }

    fn current_section(&self) -> usize {
        current_section_index(&self.sections)
    }

    fn refresh_models_if_due(&mut self) {
        match &self.model_refresh {
            ModelRefreshState::Fetching { rx, started_at } => {
                match rx.try_recv() {
                    Ok((models, errors)) => {
                        // Preserve old models on failure: only replace if the
                        // refresh returned real data
                        if !models.is_empty() {
                            self.models = models;
                            let _ = cache::save(&self.models, &errors);
                        }
                        if errors.is_empty() {
                            self.quota_retry_delay = Duration::from_secs(60);
                        } else {
                            self.quota_retry_delay =
                                (self.quota_retry_delay * 2).min(cache::TTL);
                        }
                        self.quota_errors = errors;
                        self.model_refresh = ModelRefreshState::Idle(Instant::now());
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        // Still running — but give up after 60s to avoid a
                        // hung background thread freezing the refresh forever
                        if started_at.elapsed() >= Duration::from_secs(60) {
                            self.quota_retry_delay =
                                (self.quota_retry_delay * 2).min(cache::TTL);
                            self.model_refresh = ModelRefreshState::Idle(Instant::now());
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // Background thread died without sending — treat as failure
                        self.quota_retry_delay =
                            (self.quota_retry_delay * 2).min(cache::TTL);
                        self.model_refresh = ModelRefreshState::Idle(Instant::now());
                    }
                }
            }
            ModelRefreshState::Idle(refreshed_at) => {
                let due_after = if self.quota_errors.is_empty() {
                    cache::TTL
                } else {
                    self.quota_retry_delay
                };
                if refreshed_at.elapsed() >= due_after {
                    self.model_refresh = ModelRefreshState::Fetching {
                        rx: spawn_refresh(),
                        started_at: Instant::now(),
                    };
                }
            }
        }
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
}

impl PipelineSection {
    fn done(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
        transcript: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::Done,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: transcript.into_iter().map(Into::into).collect(),
            input_placeholder: None,
        }
    }

    fn waiting_user(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
        transcript: Vec<impl Into<String>>,
        input_placeholder: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::WaitingUser,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: transcript.into_iter().map(Into::into).collect(),
            input_placeholder: Some(input_placeholder.into()),
        }
    }

    /// WaitingUser status with no input box — Enter triggers an action directly.
    fn action(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::WaitingUser,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: Vec::new(),
            input_placeholder: None,
        }
    }

    fn pending(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::Pending,
            summary: summary.into(),
            events: Vec::new(),
            transcript: Vec::new(),
            input_placeholder: None,
        }
    }

    fn running(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::Running,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: Vec::new(),
            input_placeholder: None,
        }
    }
}

impl SectionStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingUser => "waiting-user",
            Self::Done => "done",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Pending => Style::default().fg(Color::DarkGray),
            Self::Running => Style::default().fg(Color::Cyan),
            Self::WaitingUser => Style::default().fg(Color::Yellow),
            Self::Done => Style::default().fg(Color::Green),
        }
    }
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn spinner_frame(count: usize) -> &'static str {
    SPINNER[count % SPINNER.len()]
}

fn phase_done_summary(state: &RunState, phase: &str, label: &str) -> String {
    match state.phase_models.get(phase) {
        Some(pm) => format!("{label} · {} ({})", pm.model, pm.vendor),
        None => label.to_string(),
    }
}

fn build_sections(state: &RunState, window_launched: bool) -> Vec<PipelineSection> {
    let phase = state.current_phase;
    vec![
        match phase {
            Phase::IdeaInput => PipelineSection::waiting_user(
                "Idea",
                "waiting for idea",
                Vec::<String>::new(),
                Vec::<String>::new(),
                "describe what you want to build",
            ),
            _ => PipelineSection::done(
                "Idea",
                state.idea_text.as_deref().unwrap_or("idea captured"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput => PipelineSection::pending("Brainstorm", "waiting for idea"),
            Phase::BrainstormRunning => {
                if let Some(err) = &state.agent_error {
                    PipelineSection::action(
                        "Brainstorm",
                        "failed — press Enter to retry",
                        vec![
                            format!("error: {err}"),
                            format!("model: {}", state.selected_model.as_deref().unwrap_or("unknown")),
                        ],
                    )
                } else if window_launched {
                    PipelineSection::running(
                        "Brainstorm",
                        "agent running in [Brainstorm] window",
                        vec![
                            format!("model: {}", state.selected_model.as_deref().unwrap_or("unknown")),
                            "waiting for spec.md artifact".to_string(),
                        ],
                    )
                } else {
                    PipelineSection::action(
                        "Brainstorm",
                        "press Enter to run",
                        vec![
                            format!("model: {}", state.selected_model.as_deref().unwrap_or("unknown")),
                        ],
                    )
                }
            }
            _ => PipelineSection::done(
                "Brainstorm",
                phase_done_summary(state, "brainstorm", "spec written"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput | Phase::BrainstormRunning => {
                PipelineSection::pending("Spec Review", "blocked on brainstorm")
            }
            Phase::SpecReviewRunning if window_launched => PipelineSection::running(
                "Spec Review",
                "agent running in [Spec Review] window",
                vec!["waiting for spec-review.md artifact".to_string()],
            ),
            Phase::SpecReviewRunning => {
                if let Some(err) = &state.agent_error {
                    let n_done = state.spec_reviewers.len();
                    let mut events = Vec::new();
                    for (i, r) in state.spec_reviewers.iter().enumerate() {
                        events.push(format!("  ✓ round {}  {} ({})", i + 1, r.model, r.vendor));
                    }
                    if n_done > 0 {
                        events.push(String::new());
                    }
                    events.push(format!("  ✗ round {} failed: {err}", n_done + 1));
                    events.push(String::new());
                    events.push(if n_done > 0 {
                        format!("[Enter] retry  ·  [n] proceed with {n_done} review{}",
                            if n_done == 1 { "" } else { "s" })
                    } else {
                        "[Enter] retry  ·  [n] skip review, proceed to planning".to_string()
                    });
                    PipelineSection::action("Spec Review", "failed", events)
                } else {
                    PipelineSection::action(
                        "Spec Review",
                        "press Enter to run",
                        Vec::<String>::new(),
                    )
                }
            }
            Phase::SpecReviewPaused => {
                let n = state.spec_reviewers.len();
                let mut events = Vec::new();
                for (i, r) in state.spec_reviewers.iter().enumerate() {
                    events.push(format!("  ✓ round {}  {} ({})", i + 1, r.model, r.vendor));
                }
                events.push(String::new());
                events.push(format!("[Enter] add another review · [n] proceed to planning"));
                PipelineSection::action(
                    "Spec Review",
                    format!("{n} review{} done", if n == 1 { "" } else { "s" }),
                    events,
                )
            }
            _ => PipelineSection::done(
                "Spec Review",
                phase_done_summary(state, "spec-review", "review complete"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput | Phase::BrainstormRunning
            | Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                PipelineSection::pending("Planning", "blocked on spec review")
            }
            Phase::PlanningRunning if window_launched => PipelineSection::running(
                "Planning",
                "agent running in [Planning] window",
                vec!["waiting for plan.md artifact".to_string()],
            ),
            Phase::PlanningRunning => {
                if let Some(err) = &state.agent_error {
                    PipelineSection::action(
                        "Planning",
                        "failed — press Enter to retry",
                        vec![format!("error: {err}")],
                    )
                } else {
                    let n = state.spec_reviewers.len();
                    PipelineSection::action(
                        "Planning",
                        "press Enter to run",
                        vec![format!("inputs: spec + {n} review{}", if n == 1 { "" } else { "s" })],
                    )
                }
            }
            _ => PipelineSection::done(
                "Planning",
                phase_done_summary(state, "planning", "plan drafted"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::AwaitingPlanApproval => PipelineSection::waiting_user(
                "Plan Approval",
                "approval needed",
                vec!["plan is ready for review".to_string()],
                Vec::<String>::new(),
                "approve to start implementation",
            ),
            Phase::ImplementationRound(_) | Phase::ReviewRound(_) | Phase::Done => {
                PipelineSection::done(
                    "Plan Approval",
                    "approved",
                    Vec::<String>::new(),
                    Vec::<String>::new(),
                )
            }
            _ => PipelineSection::pending("Plan Approval", "blocked on planning"),
        },
        match phase {
            Phase::ImplementationRound(r) => PipelineSection::running(
                "Builder Loop",
                &format!("round {r} running"),
                vec![format!("coder round {r} in progress")],
            ),
            Phase::Done => PipelineSection::done(
                "Builder Loop",
                "complete",
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
            _ => PipelineSection::pending("Builder Loop", "blocked on approval"),
        },
    ]
}

fn vendor_from_str(s: &str) -> Option<VendorKind> {
    match s {
        "claude" => Some(VendorKind::Claude),
        "codex" => Some(VendorKind::Codex),
        "gemini" => Some(VendorKind::Gemini),
        "kimi" => Some(VendorKind::Kimi),
        _ => None,
    }
}

fn spec_review_prompt(spec_path: &str, review_path: &str) -> String {
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
"#
    )
}

fn brainstorm_prompt(idea: &str, spec_path: &str) -> String {
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
"#
    )
}

fn planning_prompt(
    spec_path: &std::path::Path,
    review_paths: &[std::path::PathBuf],
    plan_path: &std::path::Path,
) -> String {
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

When the writing-plans skill reaches the point of writing the plan
document, write it to: {plan}

IMPORTANT: Do NOT write or modify any code in the repository. Your only
output is the plan file. Implementation happens in a later phase.

The operator is here and ready to respond to your questions.
"#,
        spec = spec_path.display(),
        reviews = reviews_block,
        plan = plan_path.display(),
    )
}

fn kill_window(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-window", "-t", name])
        .output();
}

fn spawn_refresh() -> mpsc::Receiver<(Vec<ModelStatus>, Vec<QuotaError>)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(selection::load_all_models());
    });
    rx
}

fn vendor_tag(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude",
        VendorKind::Codex => "codex",
        VendorKind::Gemini => "gemini",
        VendorKind::Kimi => "kimi",
    }
}

fn vendor_color(vendor: VendorKind) -> Color {
    match vendor {
        VendorKind::Claude => Color::Magenta,
        VendorKind::Codex => Color::Green,
        VendorKind::Gemini => Color::Blue,
        VendorKind::Kimi => Color::Yellow,
    }
}

fn vendor_prefix(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude-",
        VendorKind::Codex => "gpt-",
        VendorKind::Gemini => "gemini-",
        VendorKind::Kimi => "kimi-",
    }
}

fn current_section_index(sections: &[PipelineSection]) -> usize {
    sections
        .iter()
        .position(|s| s.status == SectionStatus::WaitingUser || s.status == SectionStatus::Running)
        .or_else(|| {
            sections
                .iter()
                .position(|s| s.status == SectionStatus::Done)
                .map(|i| i.min(sections.len().saturating_sub(1)))
        })
        .unwrap_or(0)
}
