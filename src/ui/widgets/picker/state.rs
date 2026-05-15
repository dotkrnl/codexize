use super::helpers as picker_view_model;
use crate::app::palette::{self, PaletteCommand, PaletteState};
use crate::app::{Capability, KeyBinding, Severity, StatusLine, bottom_sheet, render_keymap_line};
use crate::state::{self as session_state, Modes, SessionState, Stage};
use crate::ui::chrome::{
    bottom_rule,
    modal::{render_modal_backdrop, render_modal_overlay},
    top_rule_with_left_spans,
};
use crate::ui::tui::{AppTerminal, wrap_text};
use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
#[path = "view.rs"]
mod view;
pub struct SessionEntry {
    pub session_id: String,
    pub idea_summary: String,
    pub current_stage: Stage,
    pub modes: Modes,
    pub last_modified: SystemTime,
    pub archived: bool,
}
pub struct PickerSelection {
    pub session_id: String,
    pub created: bool,
}
#[derive(Clone, Copy)]
pub(super) enum ConfirmKind {
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
    config_panel: Option<crate::ui::config_panel::ConfigPanelState>,
    /// Directory containing per-session subdirectories. Resolved from
    /// the loaded `Config` (`paths.sessions_root` when explicitly set;
    /// otherwise `state::codexize_root().join("sessions")`) so a CLI
    /// override flows through to `scan_sessions` and to any session
    /// created inline by the picker.
    sessions_root: PathBuf,
    /// `Some` when the operator explicitly set `paths.memory_root`. The
    /// inline `create_session` flow consults this when bootstrapping
    /// memory for a freshly-created session so the override wins over
    /// the session-derived default.
    memory_root_override: Option<PathBuf>,
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
        Self::new_with_paths(
            create_modes,
            crate::data::picker_io::default_sessions_root(),
            None,
        )
    }
    pub fn new_with_paths(
        create_modes: Modes,
        sessions_root: PathBuf,
        memory_root_override: Option<PathBuf>,
    ) -> Result<Self> {
        let entries = crate::data::picker_io::scan_sessions(&sessions_root)?;
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
            config_panel: None,
            sessions_root,
            memory_root_override,
        })
    }
    fn refresh(&mut self) -> Result<()> {
        self.entries = crate::data::picker_io::scan_sessions(&self.sessions_root)?;
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
                    Event::Paste(text) if self.input_mode => crate::ui::input_editor::insert_str(
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
        if let Some(panel) = self.config_panel.as_mut() {
            match panel.handle_key(key) {
                crate::ui::config_panel::PanelOutcome::KeepOpen => return Ok(KeyAction::Continue),
                crate::ui::config_panel::PanelOutcome::Close => {
                    self.config_panel = None;
                    return Ok(KeyAction::Continue);
                }
                crate::ui::config_panel::PanelOutcome::Saved => {
                    self.reload_config();
                    self.config_panel = None;
                    self.status_line.push(
                        "saved · in effect immediately".to_string(),
                        Severity::Info,
                        Duration::from_secs(3),
                    );
                    return Ok(KeyAction::Continue);
                }
            }
        }
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
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => Ok(KeyAction::Quit),
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
            KeyCode::Enter => Ok(self.handle_select()),
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
                            session_state::archive_session(&mut state);
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
                            crate::data::picker_io::delete_session(&entry.session_id)?;
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
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => {
                self.confirm_modal = None;
                Ok(KeyAction::Continue)
            }
            _ => Ok(KeyAction::Continue),
        }
    }
    fn palette_commands(&self) -> Vec<PaletteCommand> {
        picker_view_model::palette_commands(
            self.selected_entry().is_some_and(|entry| entry.archived),
        )
    }
    fn handle_palette_key(&mut self, key: KeyEvent) -> Result<KeyAction> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => {
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
            "config" => {
                let initial = match args.trim() {
                    "" => None,
                    non_empty => match crate::ui::config_panel::lookup_section(non_empty) {
                        crate::ui::config_panel::SectionLookup::Exact(name)
                        | crate::ui::config_panel::SectionLookup::UniquePrefix(name) => Some(name),
                        crate::ui::config_panel::SectionLookup::Ambiguous(matches) => {
                            self.status_line.push(
                                format!("config: ambiguous section ({})", matches.join("|")),
                                Severity::Error,
                                Duration::from_secs(4),
                            );
                            return Ok(KeyAction::Continue);
                        }
                        crate::ui::config_panel::SectionLookup::Unknown => {
                            self.status_line.push(
                                format!("config: unknown section \"{non_empty}\""),
                                Severity::Error,
                                Duration::from_secs(4),
                            );
                            return Ok(KeyAction::Continue);
                        }
                    },
                };
                let path = crate::data::config::paths::config_path();
                match crate::data::config::loader::load_from_path(&path) {
                    Ok(config) => {
                        self.config_panel =
                            Some(crate::ui::config_panel::ConfigPanelState::open_at(
                                &config, path, initial,
                            ));
                    }
                    Err(err) => {
                        self.status_line.push(
                            format!("config: {err:#}"),
                            Severity::Error,
                            Duration::from_secs(4),
                        );
                    }
                }
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
    fn reload_config(&mut self) {
        let path = crate::data::config::paths::config_path();
        if let Ok(config) = crate::data::config::loader::load_from_path(&path) {
            self.sessions_root = crate::data::picker_io::sessions_root_for(&config);
            self.memory_root_override = config
                .paths
                .memory_root
                .value()
                .parse()
                .ok()
                .filter(|p: &PathBuf| !p.as_os_str().is_empty());
            let _ = self.refresh();
        }
    }
    fn create_session_now(&mut self, idea: &str) -> Result<KeyAction> {
        let session_id = create_session(
            idea,
            self.create_modes,
            self.memory_root_override.as_deref(),
        )?;
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
        let _ = crate::ui::input_editor::apply(&mut self.input_buffer, &mut self.input_cursor, key);
        Ok(KeyAction::Continue)
    }
    fn handle_select(&self) -> KeyAction {
        if let Some(entry) = self.selected_entry() {
            KeyAction::SelectSession(PickerSelection {
                session_id: entry.session_id.clone(),
                created: false,
            })
        } else {
            KeyAction::Continue
        }
    }
    fn handle_restore(&mut self) -> Result<KeyAction> {
        if let Some(entry) = self.selected_entry()
            && entry.archived
        {
            let mut state = SessionState::load(&entry.session_id)?;
            session_state::restore_archived_session(&mut state);
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
/// Create a new session on disk and emit the standard creation events.
///
/// `idea` is stored verbatim — the caller is responsible for trimming and
/// rejecting empty input. Both the interactive picker and the direct-CLI
/// `--yolo -m` route share this helper so creation semantics (id, stage,
/// mode logging) cannot drift.
pub fn create_session(
    idea: &str,
    modes: Modes,
    memory_root_override: Option<&Path>,
) -> Result<String> {
    let session_id = generate_session_id();
    let mut state = SessionState::new(session_id.clone());
    session_state::prepare_new_session_for_brainstorm(&mut state, idea, modes);
    // Compute the newest earlier Done baseline and persist it with the new
    // session so the scheduler can gate repo-state updates later.
    let sessions_root = session_state::codexize_root().join("sessions");
    if let Ok(sessions) = crate::data::picker_io::scan_sessions_by_creation_order(&sessions_root) {
        state.planned_after_session_id =
            crate::data::picker_io::newest_earlier_done_baseline(&session_id, &sessions);
    }
    state.save()?;
    let memory_root = match memory_root_override {
        Some(root) => root.to_path_buf(),
        None => crate::logic::memory::memory_root_from_session_path(&session_state::session_dir(
            &session_id,
        )),
    };
    // Best-effort: a transient FS error here must not block session creation.
    if let Err(err) = crate::data::memory::ensure_memory_bootstrap(&memory_root) {
        let _ = state.log_event(format!("memory_bootstrap_failed: {err:#}"));
    }
    state.log_event("session created")?;
    if state.modes.yolo {
        state.log_event("mode_toggled: mode=yolo value=true source=cli")?;
    }
    if state.modes.cheap {
        state.log_event("mode_toggled: mode=cheap value=true source=cli")?;
    }
    Ok(session_id)
}
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn generate_session_id() -> String {
    let now: DateTime<Local> = SystemTime::now().into();
    // Include nanosecond precision plus a monotonic process counter so
    // two sessions created in rapid succession cannot collide.
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{:09}-{:04}",
        now.format("%Y%m%d-%H%M%S"),
        now.timestamp_subsec_nanos(),
        counter
    )
}
#[cfg(test)]
fn truncate_idea(idea: &Option<String>) -> String {
    match idea {
        Some(text) if text.chars().count() > 80 => {
            format!("{}...", text.chars().take(80).collect::<String>())
        }
        Some(text) => text.clone(),
        None => "(no idea yet)".to_string(),
    }
}
pub(super) fn format_relative_time(time: SystemTime, now: SystemTime) -> String {
    let duration = now.duration_since(time).unwrap_or_default();
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
pub(super) fn stage_badge(stage: Stage) -> (String, Color, &'static str) {
    match stage {
        Stage::IdeaInput => ("idea".to_string(), Color::DarkGray, "○"),
        Stage::BrainstormRunning => ("brainstorm".to_string(), Color::Cyan, "●"),
        Stage::SpecReviewRunning => ("spec review".to_string(), Color::Cyan, "●"),
        Stage::SpecReviewPaused => ("spec review".to_string(), Color::Cyan, "○"),
        Stage::PlanningRunning => ("planning".to_string(), Color::Cyan, "●"),
        Stage::PlanReviewRunning => ("plan review".to_string(), Color::Cyan, "●"),
        Stage::PlanReviewPaused => ("plan review".to_string(), Color::Cyan, "○"),
        Stage::WaitingToImplement => ("waiting".to_string(), Color::Yellow, "○"),
        Stage::RepoStateUpdateRunning => ("updating plan".to_string(), Color::Cyan, "●"),
        Stage::ShardingRunning => ("sharding".to_string(), Color::Cyan, "●"),
        Stage::ImplementationRound(n) => (format!("coding r{n}"), Color::Cyan, "●"),
        Stage::ReviewRound(n) => (format!("review r{n}"), Color::Cyan, "●"),
        Stage::BuilderRecovery(_) => ("builder recovery".to_string(), Color::Cyan, "●"),
        Stage::BuilderRecoveryPlanReview(_) => {
            ("recovery plan review".to_string(), Color::Cyan, "●")
        }
        Stage::BuilderRecoverySharding(_) => ("recovery sharding".to_string(), Color::Cyan, "●"),
        Stage::BlockedNeedsUser => ("blocked".to_string(), Color::Red, "○"),
        Stage::Done => ("done".to_string(), Color::Green, "✓"),
        Stage::Cancelled => ("cancelled".to_string(), Color::DarkGray, "✗"),
        Stage::SkipToImplPending => ("skip confirm".to_string(), Color::Yellow, "!"),
        Stage::GitGuardPending => ("guard decision".to_string(), Color::Yellow, "!"),
        Stage::FinalValidation(n) => (format!("final validation r{n}"), Color::Cyan, "●"),
        Stage::DreamingPending => ("dreaming decision".to_string(), Color::Yellow, "!"),
        Stage::Dreaming(n) => (format!("dreaming r{n}"), Color::Cyan, "●"),
        Stage::Simplification(n) => (format!("simplification r{n}"), Color::Cyan, "●"),
    }
}
pub(super) fn mode_badge_labels(modes: Modes) -> Vec<&'static str> {
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
#[path = "tests_mod.rs"]
mod tests_mod;
