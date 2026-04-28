use crate::app::palette::{self, PaletteCommand, PaletteState};
use crate::state::{Modes, Phase, SessionState};
use crate::tui::AppTerminal;
use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    fs,
    time::{Duration, SystemTime},
};

pub struct SessionEntry {
    pub session_id: String,
    pub idea_summary: String,
    pub current_phase: Phase,
    pub modes: Modes,
    pub last_modified: SystemTime,
    pub archived: bool,
}

pub struct PickerSelection {
    pub session_id: String,
    pub created: bool,
}

pub struct SessionPicker {
    entries: Vec<SessionEntry>,
    selected: usize,
    input_mode: bool,
    input_buffer: String,
    input_cursor: usize,
    show_archived: bool,
    confirm_delete_hard: bool,
    confirm_delete_soft: bool,
    create_modes: Modes,
    palette: PaletteState,
    palette_status: Option<String>,
}

enum KeyAction {
    Continue,
    SelectSession(PickerSelection),
    Quit,
}

impl SessionPicker {
    pub fn new() -> Result<Self> {
        Self::new_with_create_modes(Modes::default())
    }

    pub fn new_with_create_modes(create_modes: Modes) -> Result<Self> {
        let entries = scan_sessions()?;
        Ok(Self {
            entries,
            selected: 0,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            show_archived: false,
            confirm_delete_hard: false,
            confirm_delete_soft: false,
            create_modes,
            palette: PaletteState::default(),
            palette_status: None,
        })
    }

    fn refresh(&mut self) -> Result<()> {
        self.entries = scan_sessions()?;
        let visible_count = self.visible_entries().len();
        if self.selected >= visible_count && visible_count > 0 {
            self.selected = visible_count - 1;
        }
        Ok(())
    }

    fn visible_entries(&self) -> Vec<&SessionEntry> {
        self.entries
            .iter()
            .filter(|e| self.show_archived || !e.archived)
            .collect()
    }

    fn selected_entry(&self) -> Option<&SessionEntry> {
        let visible = self.visible_entries();
        visible.get(self.selected).copied()
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<Option<PickerSelection>> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))? {
                match event::read()? {
                    Event::Key(key) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        match self.handle_key(key)? {
                            KeyAction::Continue => continue,
                            KeyAction::SelectSession(selection) => return Ok(Some(selection)),
                            KeyAction::Quit => return Ok(None),
                        }
                    }
                    Event::Paste(text) => {
                        if self.input_mode {
                            crate::input_editor::insert_str(
                                &mut self.input_buffer,
                                &mut self.input_cursor,
                                &text,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let bottom_height = if self.input_mode {
            self.input_height(area.width, area.height)
        } else if self.palette.open {
            self.palette_overlay_height(area.height)
        } else {
            3
        };
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(bottom_height)]).split(area);

        self.draw_list(frame, chunks[0]);

        if self.input_mode {
            self.draw_input(frame, chunks[1]);
        } else {
            self.draw_action_bar(frame, chunks[1]);
        }
    }

    fn input_height(&self, total_width: u16, total_height: u16) -> u16 {
        let inner_width = total_width.saturating_sub(2).max(1) as usize;
        let cursor = self.input_cursor.min(self.input_buffer.chars().count());
        let byte = self
            .input_buffer
            .char_indices()
            .nth(cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input_buffer.len());
        let text_with_cursor = format!(
            "{}▌{}",
            &self.input_buffer[..byte],
            &self.input_buffer[byte..]
        );
        let wrapped = crate::tui::wrap_input(&text_with_cursor, inner_width).len();
        let wrapped = wrapped.max(1) as u16;
        let max = total_height.saturating_sub(3).max(3);
        (wrapped + 2).clamp(3, max)
    }

    fn draw_list(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let visible = self.visible_entries();

        if visible.is_empty() {
            let message = Paragraph::new("No sessions yet — run :new to create one")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title("Sessions"));
            frame.render_widget(message, area);
            return;
        }

        let now = SystemTime::now();
        let items: Vec<ListItem> = visible
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let (badge, color, prefix) = phase_badge(entry.current_phase);
                let time = format_relative_time(entry.last_modified, now);

                // Selected rows replace the leading spacer with `>`; unselected
                // rows keep a blank space so column alignment stays stable.
                let leading = if idx == self.selected { ">" } else { " " };
                let mut spans = vec![
                    Span::raw(leading),
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::raw(" "),
                    Span::styled(format!("{:<12}", badge), Style::default().fg(color)),
                ];
                for label in mode_badge_labels(entry.modes) {
                    let style = match label {
                        "[YOLO]" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        "[CHEAP]" => Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                        _ => Style::default().fg(Color::DarkGray),
                    };
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(label, style));
                }
                spans.extend([
                    Span::raw("  "),
                    Span::styled(format!("{:<8}", time), Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::raw(&entry.idea_summary),
                ]);
                let line = Line::from(spans);

                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Sessions"))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));

        let mut list_state = ListState::default();
        list_state.select(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut list_state);
    }

    fn draw_action_bar(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let paragraph = if self.palette.open {
            // Inner height excludes the 2 border rows. Suggestions clamp before
            // the input row so very short terminals still show the buffer.
            let inner_h = area.height.saturating_sub(2);
            Paragraph::new(self.palette_lines(area.width, inner_h))
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL).title("Command"))
        } else {
            let text = if let Some(status) = &self.palette_status {
                status.as_str()
            } else if self.confirm_delete_hard {
                "Run :delete again to permanently delete, or any other key to cancel"
            } else if self.confirm_delete_soft {
                "Run :archive again to archive, or any other key to cancel"
            } else if self.show_archived
                && self.selected_entry().map(|e| e.archived).unwrap_or(false)
            {
                "Enter continue  n new  :new  :archive  :restore  :show-archived off  :quit"
            } else if self.show_archived {
                "Enter continue  n new  :new  :archive  :show-archived off  :quit"
            } else {
                "Enter continue  n new  :new  :archive  :show-archived on  :quit"
            };

            Paragraph::new(text)
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL))
        };

        frame.render_widget(paragraph, area);
    }

    /// Inner-row count for the palette section (suggestions clamp before
    /// the input row so very short terminals still show the buffer).
    fn palette_inner_rows(&self) -> u16 {
        const MAX_OVERLAY_INNER: u16 = 12; // input + up to 10 suggestions + help
        let commands = self.palette_commands();
        let filtered = palette::filter(&self.palette.buffer, &commands);
        let suggestions = filtered.len().min(10) as u16;
        // input + suggestions + help row
        (1 + suggestions + 1).min(MAX_OVERLAY_INNER)
    }

    fn palette_overlay_height(&self, total_height: u16) -> u16 {
        // Reserve at least one row for the session list above the overlay.
        const LIST_RESERVE: u16 = 4;
        let inner = self.palette_inner_rows();
        let desired = inner + 2; // borders top/bottom
        let cap = total_height.saturating_sub(LIST_RESERVE).max(3);
        desired.min(cap).max(3)
    }

    fn palette_lines(&self, width: u16, inner_h: u16) -> Vec<Line<'static>> {
        let commands = self.palette_commands();
        let buffer = self.palette.buffer.clone();
        let ghost = palette::ghost_completion(&buffer, &commands).unwrap_or("");
        let suffix = ghost.strip_prefix(buffer.trim()).unwrap_or("");
        let mut input = vec![
            Span::styled(
                ":",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(buffer.clone()),
        ];
        if !suffix.is_empty() {
            input.push(Span::styled(
                suffix.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let mut lines: Vec<Line<'static>> = vec![Line::from(input)];
        let max = inner_h as usize;
        if max == 0 {
            return lines;
        }

        // Inner width for suggestion rows excludes the surrounding block borders.
        let inner_width = width.saturating_sub(2);

        let help = self
            .palette_status
            .clone()
            .unwrap_or_else(|| "Esc close  Tab complete  Enter run".to_string());
        let help_fits = max >= 2 && (inner_width as usize) >= help.chars().count().min(1);
        let help_reserve = if help_fits { 1 } else { 0 };
        let suggestion_capacity = max.saturating_sub(1).saturating_sub(help_reserve);

        let filtered = palette::filter(&buffer, &commands);
        for cmd in filtered.iter().take(suggestion_capacity) {
            let text = palette::suggestion_text(cmd, inner_width);
            lines.push(Line::from(Span::styled(
                text,
                Style::default().fg(Color::Gray),
            )));
        }

        if help_fits && lines.len() < max {
            lines.push(Line::from(Span::styled(
                help,
                Style::default().fg(Color::DarkGray),
            )));
        }

        lines
    }

    fn draw_input(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        // Split the buffer at the char-index cursor and render a visible
        // caret span between the two halves. This lets ratatui's wrap handle
        // positioning — no manual column math to drift.
        let cursor = self.input_cursor.min(self.input_buffer.chars().count());
        let byte = self
            .input_buffer
            .char_indices()
            .nth(cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input_buffer.len());
        let (left, right) = self.input_buffer.split_at(byte);
        let line = ratatui::text::Line::from(vec![
            ratatui::text::Span::raw(left.to_string()),
            ratatui::text::Span::styled("▌", Style::default().fg(Color::Yellow)),
            ratatui::text::Span::raw(right.to_string()),
        ]);
        let input = Paragraph::new(line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("New Session — Describe your idea...  [Enter] Create  [Esc] Cancel"),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(input, area);
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        let was_confirming = self.confirm_delete_hard || self.confirm_delete_soft;

        if self.input_mode {
            return self.handle_input_key(key);
        }

        if self.palette.open {
            return self.handle_palette_key(key);
        }

        match key.code {
            KeyCode::Esc => Ok(KeyAction::Quit),
            KeyCode::Char(':') => {
                self.palette.open();
                self.palette_status = None;
                Ok(KeyAction::Continue)
            }
            KeyCode::Up => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                if self.selected > 0 {
                    self.selected -= 1;
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Down => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                let visible_count = self.visible_entries().len();
                if self.selected + 1 < visible_count {
                    self.selected += 1;
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Enter => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                self.handle_select()
            }
            KeyCode::Char('n') => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                self.input_mode = true;
                self.input_buffer.clear();
                self.input_cursor = 0;
                Ok(KeyAction::Continue)
            }
            _ => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                Ok(KeyAction::Continue)
            }
        }
    }

    fn palette_commands(&self) -> Vec<PaletteCommand> {
        // Direct keys in the picker (see `handle_key`): `Esc` quits, `n`
        // opens the new-session input. Everything else is palette-only.
        let mut commands = vec![
            PaletteCommand {
                name: "quit",
                aliases: &["q"],
                help: "Exit picker",
                key_hint: Some("Esc"),
            },
            PaletteCommand {
                name: "new",
                aliases: &["n"],
                help: "Create a session",
                key_hint: Some("n"),
            },
            PaletteCommand {
                name: "idea",
                aliases: &["i"],
                help: "Create a session with the given idea text",
                key_hint: None,
            },
            PaletteCommand {
                name: "show-archived",
                aliases: &["a"],
                help: "Toggle archived sessions",
                key_hint: None,
            },
            PaletteCommand {
                name: "archive",
                aliases: &["d"],
                help: "Archive selected session",
                key_hint: None,
            },
            PaletteCommand {
                name: "delete",
                aliases: &["D"],
                help: "Permanently delete selected session",
                key_hint: None,
            },
        ];
        if self
            .selected_entry()
            .map(|entry| entry.archived)
            .unwrap_or(false)
        {
            commands.push(PaletteCommand {
                name: "restore",
                aliases: &["r"],
                help: "Restore selected archived session",
                key_hint: None,
            });
        }
        commands
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        match key.code {
            KeyCode::Esc => {
                self.palette.close();
                self.palette_status = None;
                Ok(KeyAction::Continue)
            }
            KeyCode::Enter => {
                let input = self.palette.buffer.clone();
                self.palette.close();
                self.execute_palette_input(&input)
            }
            KeyCode::Tab => {
                let commands = self.palette_commands();
                if let Some(ghost) = palette::ghost_completion(&self.palette.buffer, &commands) {
                    self.palette.accept_ghost(ghost);
                    self.palette_status = None;
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Backspace => {
                if self.palette.buffer.is_empty() {
                    self.palette.close();
                    self.palette_status = None;
                } else {
                    self.palette.buffer.pop();
                    self.palette.cursor = self.palette.buffer.chars().count();
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Char(c) => {
                self.palette.buffer.push(c);
                self.palette.cursor = self.palette.buffer.chars().count();
                self.palette_status = None;
                Ok(KeyAction::Continue)
            }
            _ => Ok(KeyAction::Continue),
        }
    }

    fn execute_palette_input(&mut self, input: &str) -> Result<KeyAction> {
        let commands = self.palette_commands();
        match palette::resolve(input, &commands) {
            palette::MatchResult::Exact { command, args }
            | palette::MatchResult::UniquePrefix { command, args } => {
                self.palette_status = None;
                self.execute_palette_command(command.name, &args)
            }
            palette::MatchResult::Ambiguous { candidates, .. } => {
                self.palette_status =
                    Some(format!("palette: ambiguous ({})", candidates.join("|")));
                Ok(KeyAction::Continue)
            }
            palette::MatchResult::Unknown { input } => {
                self.palette_status = Some(format!("palette: unknown command \"{input}\""));
                Ok(KeyAction::Continue)
            }
        }
    }

    fn execute_palette_command(&mut self, name: &str, args: &str) -> Result<KeyAction> {
        let was_confirming = self.confirm_delete_hard || self.confirm_delete_soft;
        match name {
            "quit" => Ok(KeyAction::Quit),
            "new" | "idea" => {
                self.confirm_delete_hard = false;
                self.confirm_delete_soft = false;
                let trimmed = args.trim();
                if !trimmed.is_empty() {
                    return self.create_session_now(trimmed);
                }
                self.input_mode = true;
                self.input_buffer.clear();
                self.input_cursor = 0;
                Ok(KeyAction::Continue)
            }
            "show-archived" => {
                self.confirm_delete_hard = false;
                self.confirm_delete_soft = false;
                match args.trim() {
                    "on" => self.show_archived = true,
                    "off" => self.show_archived = false,
                    _ => self.show_archived = !self.show_archived,
                }
                self.selected = 0;
                Ok(KeyAction::Continue)
            }
            "archive" => self.handle_soft_delete(was_confirming),
            "delete" => self.handle_hard_delete(was_confirming),
            "restore" => {
                self.confirm_delete_hard = false;
                self.confirm_delete_soft = false;
                self.handle_restore()
            }
            _ => Ok(KeyAction::Continue),
        }
    }

    fn create_session_now(&mut self, idea: &str) -> Result<KeyAction> {
        let session_id = create_session(idea, self.create_modes)?;
        Ok(KeyAction::SelectSession(PickerSelection {
            session_id,
            created: true,
        }))
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = false;
                return Ok(KeyAction::Continue);
            }
            KeyCode::Enter => {
                let trimmed = self.input_buffer.trim().to_string();
                if !trimmed.is_empty() {
                    return self.create_session_now(&trimmed);
                }
                self.input_mode = false;
                return Ok(KeyAction::Continue);
            }
            _ => {}
        }
        let _ = crate::input_editor::apply(&mut self.input_buffer, &mut self.input_cursor, key);
        Ok(KeyAction::Continue)
    }

    fn handle_select(&self) -> Result<KeyAction> {
        if let Some(entry) = self.selected_entry() {
            Ok(KeyAction::SelectSession(PickerSelection {
                session_id: entry.session_id.clone(),
                created: false,
            }))
        } else {
            Ok(KeyAction::Continue)
        }
    }

    fn handle_soft_delete(&mut self, was_confirming: bool) -> Result<KeyAction> {
        if was_confirming && self.confirm_delete_soft {
            if let Some(entry) = self.selected_entry() {
                let mut state = SessionState::load(&entry.session_id)?;
                state.archived = true;
                state.save()?;
                self.refresh()?;
            }
            self.confirm_delete_soft = false;
        } else {
            self.confirm_delete_soft = true;
            self.confirm_delete_hard = false;
        }
        Ok(KeyAction::Continue)
    }

    fn handle_hard_delete(&mut self, was_confirming: bool) -> Result<KeyAction> {
        if was_confirming && self.confirm_delete_hard {
            if let Some(entry) = self.selected_entry() {
                let session_dir = crate::state::session_dir(&entry.session_id);
                fs::remove_dir_all(&session_dir)?;
                self.refresh()?;
            }
            self.confirm_delete_hard = false;
        } else {
            self.confirm_delete_hard = true;
            self.confirm_delete_soft = false;
        }
        Ok(KeyAction::Continue)
    }

    fn handle_restore(&mut self) -> Result<KeyAction> {
        if let Some(entry) = self.selected_entry()
            && entry.archived
        {
            let mut state = SessionState::load(&entry.session_id)?;
            state.archived = false;
            state.save()?;
            self.refresh()?;
        }
        Ok(KeyAction::Continue)
    }
}

pub fn scan_sessions() -> Result<Vec<SessionEntry>> {
    let sessions_dir = crate::state::codexize_root().join("sessions");

    if !sessions_dir.exists() {
        fs::create_dir_all(&sessions_dir)?;
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();

    for entry in fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let session_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let toml_path = path.join("session.toml");
        if !toml_path.exists() {
            continue;
        }

        let state = match SessionState::load(&session_id) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let last_modified = fs::metadata(&toml_path)?.modified()?;

        entries.push(SessionEntry {
            session_id,
            idea_summary: state
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| truncate_idea(&state.idea_text)),
            current_phase: state.current_phase,
            modes: state.modes,
            last_modified,
            archived: state.archived,
        });
    }

    entries.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(entries)
}

/// Create a new session on disk and emit the standard creation events.
///
/// `idea` is stored verbatim — the caller is responsible for trimming and
/// rejecting empty input. Both the interactive picker and the direct-CLI
/// `--yolo -m` route share this helper so creation semantics (id, phase,
/// mode logging) cannot drift.
pub fn create_session(idea: &str, modes: Modes) -> Result<String> {
    let session_id = generate_session_id();
    let mut state = SessionState::new(session_id.clone());
    state.modes = modes;
    state.idea_text = Some(idea.to_string());
    state.current_phase = Phase::BrainstormRunning;
    state.save()?;
    state.log_event("session created")?;
    if state.modes.yolo {
        state.log_event("mode_toggled: mode=yolo value=true source=cli")?;
    }
    if state.modes.cheap {
        state.log_event("mode_toggled: mode=cheap value=true source=cli")?;
    }
    Ok(session_id)
}

pub fn generate_session_id() -> String {
    let now: DateTime<Local> = SystemTime::now().into();
    // Include nanosecond precision so two sessions created in the same
    // wall-clock second cannot collide on the session directory name.
    now.format("%Y%m%d-%H%M%S-%9f").to_string()
}

fn truncate_idea(idea: &Option<String>) -> String {
    match idea {
        Some(text) if text.chars().count() > 80 => {
            format!("{}...", text.chars().take(80).collect::<String>())
        }
        Some(text) => text.clone(),
        None => "(no idea yet)".to_string(),
    }
}

fn format_relative_time(time: SystemTime, now: SystemTime) -> String {
    let duration = now.duration_since(time).unwrap_or_default();
    let secs = duration.as_secs();

    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn phase_badge(phase: Phase) -> (String, Color, &'static str) {
    match phase {
        Phase::IdeaInput => ("idea".to_string(), Color::DarkGray, "○"),
        Phase::BrainstormRunning => ("brainstorm".to_string(), Color::Cyan, "●"),
        Phase::SpecReviewRunning => ("spec review".to_string(), Color::Cyan, "●"),
        Phase::SpecReviewPaused => ("spec review".to_string(), Color::Cyan, "○"),
        Phase::PlanningRunning => ("planning".to_string(), Color::Cyan, "●"),
        Phase::PlanReviewRunning => ("plan review".to_string(), Color::Cyan, "●"),
        Phase::PlanReviewPaused => ("plan review".to_string(), Color::Cyan, "○"),
        Phase::ShardingRunning => ("sharding".to_string(), Color::Cyan, "●"),
        Phase::ImplementationRound(n) => (format!("coding r{}", n), Color::Cyan, "●"),
        Phase::ReviewRound(n) => (format!("review r{}", n), Color::Cyan, "●"),
        Phase::BuilderRecovery(_) => ("builder recovery".to_string(), Color::Cyan, "●"),
        Phase::BuilderRecoveryPlanReview(_) => {
            ("recovery plan review".to_string(), Color::Cyan, "●")
        }
        Phase::BuilderRecoverySharding(_) => ("recovery sharding".to_string(), Color::Cyan, "●"),
        Phase::BlockedNeedsUser => ("blocked".to_string(), Color::Red, "○"),
        Phase::Done => ("done".to_string(), Color::Green, "✓"),
        Phase::SkipToImplPending => ("skip confirm".to_string(), Color::Yellow, "!"),
        Phase::GitGuardPending => ("guard decision".to_string(), Color::Yellow, "!"),
    }
}

fn mode_badge_labels(modes: Modes) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if modes.yolo {
        labels.push("[YOLO]");
    }
    if modes.cheap {
        labels.push("[CHEAP]");
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_picker(input_buffer: &str, input_cursor: usize) -> SessionPicker {
        SessionPicker {
            entries: Vec::new(),
            selected: 0,
            input_mode: true,
            input_buffer: input_buffer.to_string(),
            input_cursor,
            show_archived: false,
            confirm_delete_hard: false,
            confirm_delete_soft: false,
            create_modes: crate::state::Modes::default(),
            palette: PaletteState::default(),
            palette_status: None,
        }
    }

    #[test]
    fn input_height_counts_cursor_wrap_at_line_end() {
        let picker = test_picker("abcd", 4);

        assert_eq!(picker.input_height(6, 10), 4);
    }

    #[test]
    fn test_idea_summary_truncates() {
        let long_text = "a".repeat(100);
        let summary = truncate_idea(&Some(long_text));
        assert!(summary.len() <= 83); // 80 + "..."
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_idea_summary_short() {
        let summary = truncate_idea(&Some("hello world".to_string()));
        assert_eq!(summary, "hello world");
    }

    #[test]
    fn test_idea_summary_fallback() {
        let summary = truncate_idea(&None);
        assert_eq!(summary, "(no idea yet)");
    }

    #[test]
    fn test_relative_time_seconds() {
        let now = SystemTime::now();
        let ago = now - Duration::from_secs(45);
        let formatted = format_relative_time(ago, now);
        assert_eq!(formatted, "45s ago");
    }

    #[test]
    fn test_relative_time_minutes() {
        let now = SystemTime::now();
        let ago = now - Duration::from_secs(150);
        let formatted = format_relative_time(ago, now);
        assert_eq!(formatted, "2m ago");
    }

    #[test]
    fn test_relative_time_hours() {
        let now = SystemTime::now();
        let ago = now - Duration::from_secs(7200);
        let formatted = format_relative_time(ago, now);
        assert_eq!(formatted, "2h ago");
    }

    #[test]
    fn test_relative_time_days() {
        let now = SystemTime::now();
        let ago = now - Duration::from_secs(86400 * 3);
        let formatted = format_relative_time(ago, now);
        assert_eq!(formatted, "3d ago");
    }

    #[test]
    fn test_generate_session_id() {
        let id = generate_session_id();
        assert_eq!(id.len(), 25); // YYYYMMDD-HHMMSS-NNNNNNNNN
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
        assert_eq!(parts[2].len(), 9);
        assert!(parts[2].chars().all(|c| c.is_ascii_digit()));
    }

    fn with_temp_codexize_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("CODEXIZE_ROOT");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
                None => std::env::remove_var("CODEXIZE_ROOT"),
            }
        }
        outcome.unwrap()
    }

    #[test]
    fn scan_sessions_returns_empty_when_root_is_brand_new() {
        with_temp_codexize_root(|| {
            // First call creates the sessions dir on demand and returns no entries.
            let entries = scan_sessions().unwrap();
            assert!(entries.is_empty());
            assert!(crate::state::codexize_root().join("sessions").exists());
        });
    }

    #[test]
    fn scan_sessions_skips_directories_without_session_toml() {
        with_temp_codexize_root(|| {
            let _ = scan_sessions().unwrap();
            let stray = crate::state::codexize_root().join("sessions").join("stray");
            fs::create_dir_all(&stray).unwrap();
            // No session.toml inside; the entry must be ignored.
            let entries = scan_sessions().unwrap();
            assert!(
                entries.is_empty(),
                "stray dir without session.toml must be skipped"
            );
        });
    }

    #[test]
    fn scan_sessions_returns_entries_sorted_by_recency() {
        with_temp_codexize_root(|| {
            // Stage two sessions; touching their session.toml gives the
            // newer one a more recent mtime so it sorts first.
            let mut older = SessionState::new("alpha".to_string());
            older.title = Some("alpha title".to_string());
            older.save().unwrap();
            // Sleep enough to ensure the next save's mtime strictly exceeds
            // alpha's (mtime resolution is filesystem-dependent).
            std::thread::sleep(std::time::Duration::from_millis(10));
            let mut newer = SessionState::new("beta".to_string());
            newer.title = Some("beta title".to_string());
            newer.save().unwrap();

            let entries = scan_sessions().unwrap();
            assert_eq!(entries.len(), 2, "both sessions must be discovered");
            assert_eq!(entries[0].session_id, "beta", "newest first");
            assert_eq!(entries[1].session_id, "alpha");
            assert_eq!(entries[0].idea_summary, "beta title");
        });
    }

    #[test]
    fn new_session_seeds_create_modes() {
        with_temp_codexize_root(|| {
            let mut picker = SessionPicker::new_with_create_modes(crate::state::Modes {
                yolo: false,
                cheap: true,
            })
            .unwrap();
            picker.input_mode = true;
            picker.input_buffer = "ship cheap mode".to_string();
            picker.input_cursor = picker.input_buffer.chars().count();

            let action = picker
                .handle_input_key(KeyEvent::new(
                    KeyCode::Enter,
                    crossterm::event::KeyModifiers::NONE,
                ))
                .unwrap();
            let KeyAction::SelectSession(selection) = action else {
                panic!("expected new session selection");
            };

            assert!(selection.created);
            let state = SessionState::load(&selection.session_id).expect("load new session");
            assert!(state.modes.cheap);
        });
    }

    #[test]
    fn direct_n_key_enters_input_mode_outside_palette() {
        let mut picker = test_picker("", 0);
        picker.input_mode = false;

        assert!(matches!(
            picker
                .handle_key(KeyEvent::new(
                    KeyCode::Char('n'),
                    crossterm::event::KeyModifiers::NONE,
                ))
                .unwrap(),
            KeyAction::Continue
        ));
        assert!(picker.input_mode, "bare n should enter input mode");
        assert!(
            picker.input_buffer.is_empty(),
            "buffer should be empty on entry"
        );

        // Esc exits without creating a session
        assert!(matches!(
            picker
                .handle_key(KeyEvent::new(
                    KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                ))
                .unwrap(),
            KeyAction::Continue
        ));
        assert!(!picker.input_mode, "Esc should exit input mode");
    }

    #[test]
    fn direct_a_q_keys_remain_palette_only() {
        let mut picker = test_picker("", 0);
        picker.input_mode = false;

        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char('a'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert!(
            !picker.show_archived,
            "archive visibility action must be routed through the palette"
        );

        assert!(matches!(
            picker
                .handle_key(KeyEvent::new(
                    KeyCode::Char('q'),
                    crossterm::event::KeyModifiers::NONE,
                ))
                .unwrap(),
            KeyAction::Continue
        ));
    }

    #[test]
    fn input_mode_keeps_colon_literal() {
        let mut picker = test_picker("", 0);

        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(':'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();

        assert_eq!(picker.input_buffer, ":");
    }

    #[test]
    fn palette_overlay_empty_buffer_lists_commands_in_picker() {
        let mut picker = test_picker("", 0);
        picker.input_mode = false;
        picker.palette.open();

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..24)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        // Empty buffer lists every command with help text.
        assert!(text.contains("quit"), "lists quit");
        assert!(text.contains("Exit picker"));
        assert!(text.contains("new"));
        assert!(text.contains("Create a session"));
        assert!(text.contains("idea"));
        assert!(text.contains("show-archived"));

        // The picker `new` has a real direct key (`n`), so its shortcut renders.
        // `idea`/`archive`/`delete` are palette-only, no shortcut text.
        // Strip the surrounding `│` border characters before inspecting trailers.
        let strip_borders = |row: &str| -> String {
            row.trim_matches(|c: char| c == '│' || c == ' ')
                .trim_end()
                .to_string()
        };

        let new_row = text
            .lines()
            .find(|l| l.contains("Create a session"))
            .expect("new row present");
        assert!(
            strip_borders(new_row).ends_with('n'),
            "new advertises shortcut n: {new_row:?}"
        );

        // Idea has no direct key in the picker; the suggestion text must omit
        // the shortcut entirely. Inspect the rendered cell content directly to
        // avoid coupling to padding inside the bordered overlay.
        let commands = picker.palette_commands();
        let idea = commands
            .iter()
            .find(|c| c.name == "idea")
            .expect("idea command present");
        let idea_text = palette::suggestion_text(idea, 78);
        assert!(
            !idea_text.contains(" i\u{0}") && !idea_text.trim_end().ends_with('i'),
            "idea suggestion must not advertise a shortcut hint: {idea_text:?}"
        );
    }

    #[test]
    fn palette_overlay_filters_and_resolves_alias_in_picker() {
        let mut picker = test_picker("", 0);
        picker.input_mode = false;
        picker.palette.open();
        picker.palette.buffer = "ar".to_string();
        picker.palette.cursor = 2;

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..24)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("archive") || text.contains("show-archived"),
            "ar prefix surfaces archive-related commands: {text}"
        );
        // Tab still resolves to the ghost completion via shared palette helpers.
        let commands = picker.palette_commands();
        let ghost = palette::ghost_completion(&picker.palette.buffer, &commands);
        assert!(
            ghost.is_some(),
            "ghost autocomplete should still resolve from prefix"
        );
    }

    #[test]
    fn action_bar_advertises_palette_commands() {
        let picker = test_picker("", 0);

        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut picker = picker;
        picker.input_mode = false;
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..8)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("n new"));
        assert!(text.contains(":new"));
        assert!(text.contains(":quit"));
        assert!(!text.contains("[n] New"));
    }

    #[test]
    fn action_bar_advertises_n_new_and_new_when_show_archived_on() {
        let mut picker = test_picker("", 0);
        picker.input_mode = false;
        picker.show_archived = true;

        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..8)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("n new"),
            "show-archived on bar should advertise n new"
        );
        assert!(
            text.contains(":new"),
            "show-archived on bar should advertise :new"
        );
    }

    #[test]
    fn palette_new_without_args_opens_input_modal() {
        let mut picker = test_picker("", 0);
        picker.input_mode = false;

        let action = picker.execute_palette_input("new").unwrap();
        assert!(matches!(action, KeyAction::Continue));
        assert!(picker.input_mode, "empty-args :new should open input modal");
        assert!(picker.input_buffer.is_empty());
    }

    #[test]
    fn palette_new_with_args_creates_session_immediately() {
        with_temp_codexize_root(|| {
            let mut picker =
                SessionPicker::new_with_create_modes(crate::state::Modes::default()).unwrap();
            picker.input_mode = false;

            let action = picker.execute_palette_input("new ship cheap mode").unwrap();
            let KeyAction::SelectSession(selection) = action else {
                panic!("expected SelectSession action");
            };
            assert!(selection.created);

            let state = SessionState::load(&selection.session_id).expect("load new session");
            assert_eq!(state.idea_text.as_deref(), Some("ship cheap mode"));
            assert_eq!(state.current_phase, Phase::BrainstormRunning);
        });
    }

    #[test]
    fn create_session_helper_persists_brainstorm_running_with_modes() {
        with_temp_codexize_root(|| {
            let session_id = create_session(
                "ship the dashboard",
                crate::state::Modes {
                    yolo: true,
                    cheap: true,
                },
            )
            .expect("create_session succeeds");

            let state = SessionState::load(&session_id).expect("load new session");
            assert_eq!(state.idea_text.as_deref(), Some("ship the dashboard"));
            assert_eq!(state.current_phase, Phase::BrainstormRunning);
            assert!(state.modes.yolo);
            assert!(state.modes.cheap);
        });
    }

    #[test]
    fn create_session_helper_logs_session_created_and_mode_events() {
        with_temp_codexize_root(|| {
            let session_id = create_session(
                "log it",
                crate::state::Modes {
                    yolo: true,
                    cheap: false,
                },
            )
            .expect("create_session succeeds");

            // The events audit trail is a TOML file next to session.toml.
            // Reading the raw file keeps the test independent of any
            // structured-log accessor.
            let events_path = crate::state::session_dir(&session_id).join("events.toml");
            let log = std::fs::read_to_string(&events_path).expect("events.toml exists");
            assert!(log.contains("session created"), "log: {log}");
            assert!(log.contains("mode=yolo"), "yolo logged: {log}");
            assert!(!log.contains("mode=cheap"), "cheap not logged: {log}");
        });
    }

    #[test]
    fn palette_idea_alias_creates_session_immediately() {
        with_temp_codexize_root(|| {
            let mut picker =
                SessionPicker::new_with_create_modes(crate::state::Modes::default()).unwrap();
            picker.input_mode = false;

            let action = picker
                .execute_palette_input("idea ship cheap mode")
                .unwrap();
            let KeyAction::SelectSession(selection) = action else {
                panic!("expected SelectSession action");
            };
            assert!(selection.created);

            let state = SessionState::load(&selection.session_id).expect("load new session");
            assert_eq!(state.idea_text.as_deref(), Some("ship cheap mode"));
            assert_eq!(state.current_phase, Phase::BrainstormRunning);
        });
    }

    #[test]
    fn mode_badge_labels_include_cheap_marker() {
        let labels = mode_badge_labels(crate::state::Modes {
            yolo: false,
            cheap: true,
        });

        assert_eq!(labels, vec!["[CHEAP]"]);
    }

    #[test]
    fn generate_session_id_distinguishes_rapid_calls() {
        // Two sessions kicked off back-to-back in the same wall-clock
        // second must produce distinct session-directory names — this used
        // to collide because the format only had second precision.
        let mut ids = std::collections::HashSet::new();
        for _ in 0..5 {
            ids.insert(generate_session_id());
        }
        assert_eq!(
            ids.len(),
            5,
            "five rapid session ids must be distinct, got {ids:?}"
        );
    }

    fn picker_with_entries(entries: Vec<SessionEntry>, selected: usize) -> SessionPicker {
        SessionPicker {
            entries,
            selected,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            show_archived: false,
            confirm_delete_hard: false,
            confirm_delete_soft: false,
            create_modes: crate::state::Modes::default(),
            palette: PaletteState::default(),
            palette_status: None,
        }
    }

    fn dummy_entry(id: &str, summary: &str) -> SessionEntry {
        SessionEntry {
            session_id: id.to_string(),
            idea_summary: summary.to_string(),
            current_phase: Phase::IdeaInput,
            modes: crate::state::Modes::default(),
            last_modified: SystemTime::now(),
            archived: false,
        }
    }

    #[test]
    fn selected_row_uses_marker_and_no_reversed_style() {
        let picker = picker_with_entries(
            vec![
                dummy_entry("alpha", "first idea"),
                dummy_entry("beta", "second idea"),
            ],
            1,
        );

        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();

        // Locate the row carrying each idea and inspect its leading marker
        // and style. Row index is independent of the surrounding border.
        let mut alpha_row = None;
        let mut beta_row = None;
        for y in 0..8 {
            let row: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
            if row.contains("first idea") {
                alpha_row = Some(y);
            }
            if row.contains("second idea") {
                beta_row = Some(y);
            }
        }
        let alpha_y = alpha_row.expect("alpha row rendered");
        let beta_y = beta_row.expect("beta row rendered");

        // The first text cell after the list border must be the focus marker
        // for the selected row and a blank for unselected rows.
        let cell_after_border = |y: u16| -> String { buf[(1, y)].symbol().to_string() };
        assert_eq!(
            cell_after_border(alpha_y),
            " ",
            "unselected row stays blank"
        );
        assert_eq!(
            cell_after_border(beta_y),
            ">",
            "selected row shows > marker"
        );

        // Selected row must not rely on reversed background. Scan every cell
        // on the selected row to confirm REVERSED is absent.
        for x in 0..80 {
            let style = buf[(x, beta_y)].style();
            assert!(
                !style.add_modifier.contains(Modifier::REVERSED),
                "selected row must not use Modifier::REVERSED at col {x}"
            );
        }
    }

    #[test]
    fn selected_row_highlight_style_excludes_reversed() {
        // Even outside of rendering, the highlight style itself must be free
        // of REVERSED so any future render path inherits the same contract.
        let picker = picker_with_entries(vec![dummy_entry("alpha", "only idea")], 0);
        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        for y in 0..8 {
            for x in 0..80 {
                assert!(
                    !buf[(x, y)]
                        .style()
                        .add_modifier
                        .contains(Modifier::REVERSED),
                    "no cell may render with REVERSED at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn test_phase_badge_variants() {
        let (badge, color, prefix) = phase_badge(Phase::Done);
        assert_eq!(badge, "done");
        assert_eq!(color, Color::Green);
        assert_eq!(prefix, "✓");

        let (badge, _, prefix) = phase_badge(Phase::BlockedNeedsUser);
        assert_eq!(badge, "blocked");
        assert_eq!(prefix, "○");

        let (badge, _, _) = phase_badge(Phase::ImplementationRound(3));
        assert_eq!(badge, "coding r3");
    }
}
