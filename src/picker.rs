use crate::state::{Phase, SessionState};
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
    pub last_modified: SystemTime,
    pub archived: bool,
}

pub struct SessionPicker {
    entries: Vec<SessionEntry>,
    selected: usize,
    input_mode: bool,
    input_buffer: String,
    show_archived: bool,
    confirm_delete_hard: bool,
    confirm_delete_soft: bool,
}

enum KeyAction {
    Continue,
    SelectSession(String),
    Quit,
}

impl SessionPicker {
    pub fn new() -> Result<Self> {
        let entries = scan_sessions()?;
        Ok(Self {
            entries,
            selected: 0,
            input_mode: false,
            input_buffer: String::new(),
            show_archived: false,
            confirm_delete_hard: false,
            confirm_delete_soft: false,
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

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<Option<String>> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match self.handle_key(key)? {
                    KeyAction::Continue => continue,
                    KeyAction::SelectSession(id) => return Ok(Some(id)),
                    KeyAction::Quit => return Ok(None),
                }
            }
        }
    }

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let bottom_height = if self.input_mode {
            self.input_height(area.width, area.height)
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
        let mut wrapped: usize = 0;
        for segment in self.input_buffer.split('\n') {
            let len = segment.chars().count();
            wrapped += len.div_ceil(inner_width).max(1);
        }
        let wrapped = wrapped.max(1) as u16;
        let max = total_height.saturating_sub(3).max(3);
        (wrapped + 2).clamp(3, max)
    }

    fn draw_list(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let visible = self.visible_entries();

        if visible.is_empty() {
            let message = Paragraph::new("No sessions yet — press [n] to create one")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title("Sessions"));
            frame.render_widget(message, area);
            return;
        }

        let now = SystemTime::now();
        let items: Vec<ListItem> = visible
            .iter()
            .map(|entry| {
                let (badge, color, prefix) = phase_badge(entry.current_phase);
                let time = format_relative_time(entry.last_modified, now);

                let line = Line::from(vec![
                    Span::raw(" "),
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::raw(" "),
                    Span::styled(format!("{:<12}", badge), Style::default().fg(color)),
                    Span::raw("  "),
                    Span::styled(format!("{:<8}", time), Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::raw(&entry.idea_summary),
                ]);

                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Sessions"))
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        let mut list_state = ListState::default();
        list_state.select(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut list_state);
    }

    fn draw_action_bar(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let text = if self.confirm_delete_hard {
            "Press [D] again to permanently delete, or any other key to cancel"
        } else if self.confirm_delete_soft {
            "Press [d] again to archive, or any other key to cancel"
        } else if self.show_archived && self.selected_entry().map(|e| e.archived).unwrap_or(false) {
            "[Enter] Continue  [n] New  [d] Delete  [r] Restore  [a] Hide archived  [q] Quit"
        } else if self.show_archived {
            "[Enter] Continue  [n] New  [d] Delete  [a] Hide archived  [q] Quit"
        } else {
            "[Enter] Continue  [n] New  [d] Delete  [a] Show archived  [q] Quit"
        };

        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL));

        frame.render_widget(paragraph, area);
    }

    fn draw_input(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let input = Paragraph::new(self.input_buffer.as_str())
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

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Ok(KeyAction::Quit),
            KeyCode::Char('n') => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                self.input_mode = true;
                self.input_buffer.clear();
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
            KeyCode::Char('a') => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                self.show_archived = !self.show_archived;
                self.selected = 0;
                Ok(KeyAction::Continue)
            }
            KeyCode::Char('d') => self.handle_soft_delete(was_confirming),
            KeyCode::Char('D') => self.handle_hard_delete(was_confirming),
            KeyCode::Char('r') => {
                if was_confirming {
                    self.confirm_delete_hard = false;
                    self.confirm_delete_soft = false;
                }
                self.handle_restore()
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

    fn handle_input_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = false;
                Ok(KeyAction::Continue)
            }
            KeyCode::Enter => {
                if !self.input_buffer.trim().is_empty() {
                    let session_id = generate_session_id();
                    let idea_text = self.input_buffer.trim().to_string();

                    let mut state = SessionState::new(session_id.clone());
                    state.idea_text = Some(idea_text);
                    state.current_phase = Phase::BrainstormRunning;
                    state.save()?;
                    state.log_event("session created")?;

                    Ok(KeyAction::SelectSession(session_id))
                } else {
                    self.input_mode = false;
                    Ok(KeyAction::Continue)
                }
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                Ok(KeyAction::Continue)
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
                Ok(KeyAction::Continue)
            }
            _ => Ok(KeyAction::Continue),
        }
    }

    fn handle_select(&self) -> Result<KeyAction> {
        if let Some(entry) = self.selected_entry() {
            Ok(KeyAction::SelectSession(entry.session_id.clone()))
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
    let sessions_dir = std::path::PathBuf::from(".codexize/sessions");

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
            idea_summary: truncate_idea(&state.idea_text),
            current_phase: state.current_phase,
            last_modified,
            archived: state.archived,
        });
    }

    entries.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(entries)
}

pub fn generate_session_id() -> String {
    let now: DateTime<Local> = SystemTime::now().into();
    now.format("%Y%m%d-%H%M%S").to_string()
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
        Phase::BlockedNeedsUser => ("blocked".to_string(), Color::Red, "○"),
        Phase::Done => ("done".to_string(), Color::Green, "✓"),
        Phase::SkipToImplPending => ("skip confirm".to_string(), Color::Yellow, "!"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(id.len(), 15); // YYYYMMDD-HHMMSS
        assert!(id.contains('-'));
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
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
