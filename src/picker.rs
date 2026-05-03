use crate::app::chrome::{bottom_rule, modal::render_modal_overlay, top_rule_with_left_spans};
use crate::app::palette::{self, PaletteCommand, PaletteState};
use crate::app::{Capability, KeyBinding, Severity, StatusLine, bottom_sheet, render_keymap_line};
use crate::picker_view_model;
use crate::state::{self as session_state, Modes, Phase, SessionState};
use crate::tui::{AppTerminal, wrap_input};
use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use std::{
    fs,
    time::{Duration, Instant, SystemTime},
};

mod render;

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

#[derive(Clone, Copy)]
enum ConfirmKind {
    Archive,
    Delete,
}

pub struct SessionPicker {
    entries: Vec<SessionEntry>,
    selected: usize,
    viewport_top: usize,
    body_inner_height: usize,
    expanded: Option<String>,
    input_mode: bool,
    input_buffer: String,
    input_cursor: usize,
    show_archived: bool,
    confirm_modal: Option<ConfirmKind>,
    create_modes: Modes,
    palette: PaletteState,
    status_line: StatusLine,
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
            viewport_top: 0,
            body_inner_height: 0,
            expanded: None,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            show_archived: false,
            confirm_modal: None,
            create_modes,
            palette: PaletteState::default(),
            status_line: StatusLine::new(),
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
        picker_view_model::visible_entries(&self.entries, self.show_archived)
    }

    fn selected_entry(&self) -> Option<&SessionEntry> {
        picker_view_model::selected_entry(&self.entries, self.show_archived, self.selected)
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<Option<PickerSelection>> {
        loop {
            self.status_line.tick(Instant::now());
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
                    Event::Paste(text) if self.input_mode => crate::input_editor::insert_str(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        &text,
                    ),
                    Event::Paste(_) => {}
                    _ => {}
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        if let Some(kind) = self.confirm_modal {
            return self.handle_modal_key(key, kind);
        }

        if self.input_mode {
            return self.handle_input_key(key);
        }

        if self.palette.open {
            return self.handle_palette_key(key);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => Ok(KeyAction::Quit),
            KeyCode::Char(':') => {
                self.palette.open();
                self.status_line.clear();
                Ok(KeyAction::Continue)
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.expanded.is_some()
                        && self.expanded != self.selected_entry().map(|e| e.session_id.clone())
                    {
                        self.expanded = None;
                    }
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Down => {
                let visible_count = self.visible_entries().len();
                if self.selected + 1 < visible_count {
                    self.selected += 1;
                    if self.expanded.is_some()
                        && self.expanded != self.selected_entry().map(|e| e.session_id.clone())
                    {
                        self.expanded = None;
                    }
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::PageUp => {
                let step = self.page_step();
                self.selected = self.selected.saturating_sub(step);
                if self.expanded.is_some()
                    && self.expanded != self.selected_entry().map(|e| e.session_id.clone())
                {
                    self.expanded = None;
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::PageDown => {
                let visible_count = self.visible_entries().len();
                let step = self.page_step();
                self.selected = (self.selected + step).min(visible_count.saturating_sub(1));
                if self.expanded.is_some()
                    && self.expanded != self.selected_entry().map(|e| e.session_id.clone())
                {
                    self.expanded = None;
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Char(' ') => {
                if let Some(entry) = self.selected_entry() {
                    let id = entry.session_id.clone();
                    if self.expanded.as_ref() == Some(&id) {
                        self.expanded = None;
                    } else {
                        self.expanded = Some(id);
                    }
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Enter => self.handle_select(),
            KeyCode::Char('n') => {
                self.input_mode = true;
                self.input_buffer.clear();
                self.input_cursor = 0;
                Ok(KeyAction::Continue)
            }
            _ => Ok(KeyAction::Continue),
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent, kind: ConfirmKind) -> Result<KeyAction> {
        match key.code {
            KeyCode::Enter => {
                match kind {
                    ConfirmKind::Archive => {
                        if let Some(entry) = self.selected_entry() {
                            let mut state = SessionState::load(&entry.session_id)?;
                            session_state::transitions::archive_session(&mut state);
                            state.save()?;
                            self.refresh()?;
                            self.status_line.push(
                                "Session archived".to_string(),
                                Severity::Info,
                                Duration::from_secs(3),
                            );
                        }
                    }
                    ConfirmKind::Delete => {
                        if let Some(entry) = self.selected_entry() {
                            let session_dir = crate::state::session_dir(&entry.session_id);
                            fs::remove_dir_all(&session_dir)?;
                            self.refresh()?;
                            self.status_line.push(
                                "Session deleted".to_string(),
                                Severity::Info,
                                Duration::from_secs(3),
                            );
                        }
                    }
                }
                self.confirm_modal = None;
                Ok(KeyAction::Continue)
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.confirm_modal = None;
                Ok(KeyAction::Continue)
            }
            _ => Ok(KeyAction::Continue),
        }
    }

    fn palette_commands(&self) -> Vec<PaletteCommand> {
        picker_view_model::palette_commands(
            self.selected_entry()
                .map(|entry| entry.archived)
                .unwrap_or(false),
        )
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.palette.close();
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
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Backspace => {
                if self.palette.buffer.is_empty() {
                    self.palette.close();
                } else {
                    self.palette.buffer.pop();
                    self.palette.cursor = self.palette.buffer.chars().count();
                }
                Ok(KeyAction::Continue)
            }
            KeyCode::Char(c) => {
                self.palette.buffer.push(c);
                self.palette.cursor = self.palette.buffer.chars().count();
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
                self.execute_palette_command(command.name, &args)
            }
            palette::MatchResult::Ambiguous { candidates, .. } => {
                self.status_line.push(
                    format!("palette: ambiguous ({})", candidates.join("|")),
                    Severity::Error,
                    Duration::from_secs(3),
                );
                Ok(KeyAction::Continue)
            }
            palette::MatchResult::Unknown { input } => {
                self.status_line.push(
                    format!("palette: unknown command \"{input}\""),
                    Severity::Error,
                    Duration::from_secs(3),
                );
                Ok(KeyAction::Continue)
            }
        }
    }

    fn execute_palette_command(&mut self, name: &str, args: &str) -> Result<KeyAction> {
        match name {
            "quit" => Ok(KeyAction::Quit),
            "new" | "idea" => {
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
                match args.trim() {
                    "on" => self.show_archived = true,
                    "off" => self.show_archived = false,
                    _ => self.show_archived = !self.show_archived,
                }
                self.selected = 0;
                Ok(KeyAction::Continue)
            }
            "archive" => {
                self.confirm_modal = Some(ConfirmKind::Archive);
                Ok(KeyAction::Continue)
            }
            "delete" => {
                self.confirm_modal = Some(ConfirmKind::Delete);
                Ok(KeyAction::Continue)
            }
            "restore" => self.handle_restore(),
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

    fn handle_restore(&mut self) -> Result<KeyAction> {
        if let Some(entry) = self.selected_entry()
            && entry.archived
        {
            let mut state = SessionState::load(&entry.session_id)?;
            session_state::transitions::restore_archived_session(&mut state);
            state.save()?;
            self.refresh()?;
            self.status_line.push(
                "Session restored".to_string(),
                Severity::Info,
                Duration::from_secs(3),
            );
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

    entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_modified));

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
    session_state::transitions::prepare_new_session_for_brainstorm(&mut state, idea, modes);
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
        Phase::FinalValidation(n) => (format!("final validation r{}", n), Color::Cyan, "●"),
        Phase::Simplification(n) => (format!("simplification r{}", n), Color::Cyan, "●"),
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
mod tests_mod;
