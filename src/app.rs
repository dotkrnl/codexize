use crate::{
    cache,
    selection::{self, ModelStatus, VendorKind},
    state::RunState,
    tmux::TmuxContext,
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
    WaitingUser,
    Done,
}

#[derive(Debug)]
enum ModelRefreshState {
    /// Background fetch is running; receiver will deliver results
    Fetching(mpsc::Receiver<Vec<ModelStatus>>),
    /// Last completed refresh; re-trigger after TTL
    Idle(Instant),
}

impl App {
    pub fn new(tmux: TmuxContext, state: RunState) -> Self {
        let sections = vec![
            PipelineSection::done(
                "Idea",
                "idea captured",
                vec![
                    "operator described the Rust TUI rewrite",
                    "scope narrowed to pipeline-first control",
                    "tmux windows chosen over split panes",
                ],
                vec![
                    "> let's completely rewrite it into a Rust TUI",
                    "< captured tmux-first orchestration goals",
                ],
            ),
            PipelineSection::done(
                "Brainstorm",
                "spec written",
                vec![
                    "brainstorm agent flow simplified",
                    "artifact-first control approved",
                    "spec written to plans/2026-04-23-codexize-rust-tui-design.md",
                ],
                vec![
                    "> pure tmux control, no ACP",
                    "> compiled adapter",
                    "< spec updated with wrapper-owned execution",
                ],
            ),
            PipelineSection::done(
                "Spec Review",
                "2 issues resolved",
                vec![
                    "reviewer requested stricter stop-gate ownership",
                    "runtime state moved under .codexize/runs/<run-id>",
                    "spec review closed",
                ],
                vec![
                    "< stop hooks must never advance the pipeline",
                    "> yes - hooks lookup the states here to decide allow it to stop or not",
                ],
            ),
            PipelineSection::done(
                "Planning",
                "plan drafted",
                vec![
                    "mvp reduced to real tmux + real provider CLI",
                    "Rust TUI shell prioritized before adapters",
                    "runtime and controller work queued next",
                ],
                vec![
                    "< implementation loop split into scaffold, state, tmux, adapter",
                    "> work on this together one piece by one piece",
                ],
            ),
            PipelineSection::done(
                "Plan Reviews",
                "both reviews passed",
                vec![
                    "review 1 accepted pipeline-first summary",
                    "review 2 accepted compiled adapter direction",
                    "awaiting explicit approval gate",
                ],
                vec!["< both plan reviews converged on the same MVP slice"],
            ),
            PipelineSection::waiting_user(
                "Plan Approval",
                "approval needed",
                vec![
                    "layout refactor proposal approved in chat",
                    "single-column accordion selected",
                    "waiting for operator input before advancing",
                ],
                vec!["> implement it"],
                "type approval or next instruction here",
            ),
            PipelineSection::pending("Builder Loop", "blocked on approval"),
        ];

        let current = current_section_index(&sections);

        // Load cached models immediately so the UI is populated on first frame
        let (models, model_refresh) = match cache::load() {
            Some((cached, expired)) => {
                let refresh = if expired {
                    ModelRefreshState::Fetching(spawn_refresh())
                } else {
                    ModelRefreshState::Idle(Instant::now())
                };
                (cached, refresh)
            }
            None => (Vec::new(), ModelRefreshState::Fetching(spawn_refresh())),
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
            section_scroll: vec![usize::MAX; 7],
            body_inner_height: 0,
            input_mode: false,
            input_buffer: String::new(),
        }
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        loop {
            self.refresh_models_if_due();
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
        let model_height = (self.models.len() as u16).max(1) + 2;
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

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
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
                if self.can_focus_input() {
                    self.input_mode = true;
                } else {
                    self.toggle_selected_section();
                }
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
        self.model_refresh = ModelRefreshState::Fetching(spawn_refresh());
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
                " | Up/Down Enter t PgUp/PgDn q",
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
                    .unwrap_or_else(|| "--".to_string());
                let quota = model
                    .quota_percent
                    .map(|v| format!("{v:>3}%"))
                    .unwrap_or_else(|| " --".to_string());

                // Vendor tag only on first row of each group; blank pad on rest
                let tag_span = if i == 0 {
                    Span::styled(
                        format!("{:<6}", tag),
                        Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("      ")
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

        Paragraph::new(lines).block(Block::default().title("Models").borders(Borders::ALL))
    }

    fn section_header(
        &self,
        index: usize,
        expanded: bool,
        section: &PipelineSection,
    ) -> Line<'static> {
        let marker = if expanded { "v" } else { ">" };
        let style = if index == self.selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::raw(format!("{marker} ")),
            Span::raw(section.name.clone()),
            Span::raw(" | "),
            Span::styled(section.status.label(), section.status.style()),
            Span::raw(" | "),
            Span::styled(section.summary.clone(), Style::default().fg(Color::Gray)),
        ])
        .style(style)
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
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  input",
                if self.input_mode && index == self.selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Yellow)
                },
            )));
            let text = if self.input_buffer.is_empty() {
                placeholder.to_string()
            } else {
                self.input_buffer.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if self.input_mode && index == self.selected {
                        "  * "
                    } else {
                        "  > "
                    },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    text,
                    if self.input_buffer.is_empty() {
                        Style::default().fg(Color::Gray)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
            ]));
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
            ModelRefreshState::Fetching(rx) => {
                // Non-blocking check: take results if the background thread finished
                if let Ok(models) = rx.try_recv() {
                    let _ = cache::save(&models);
                    self.models = models;
                    self.model_refresh = ModelRefreshState::Idle(Instant::now());
                }
            }
            ModelRefreshState::Idle(refreshed_at) => {
                if refreshed_at.elapsed() >= cache::TTL {
                    self.model_refresh = ModelRefreshState::Fetching(spawn_refresh());
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
}

impl SectionStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::WaitingUser => "waiting-user",
            Self::Done => "done",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Pending => Style::default().fg(Color::DarkGray),
            Self::WaitingUser => Style::default().fg(Color::Yellow),
            Self::Done => Style::default().fg(Color::Green),
        }
    }
}

fn spawn_refresh() -> mpsc::Receiver<Vec<ModelStatus>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let models = selection::load_all_models();
        let _ = tx.send(models);
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
        .position(|section| section.status == SectionStatus::WaitingUser)
        .or_else(|| {
            sections
                .iter()
                .position(|section| section.status != SectionStatus::Pending)
        })
        .unwrap_or(0)
}
