use crate::app::chrome::{bottom_rule, modal::render_modal_overlay, top_rule_with_left_spans};
use crate::app::palette::{self, PaletteCommand, PaletteState};
use crate::app::{Capability, KeyBinding, Severity, StatusLine, bottom_sheet, render_keymap_line};
use crate::state::{Modes, Phase, SessionState};
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

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let term_h = area.height;
        let width = area.width;
        let degenerate = term_h < 10;

        let top_rule_right = if self.show_archived {
            Some("showing archived".to_string())
        } else {
            None
        };
        let top_rule = top_rule_with_left_spans(
            vec![Span::styled(
                "Sessions",
                Style::default().fg(Color::DarkGray),
            )],
            top_rule_right.as_deref(),
            width,
        );

        let bottom_rule = bottom_rule(width, None);

        let status_content = if degenerate {
            None
        } else {
            self.status_line.render()
        };
        let status_h = if status_content.is_some() { 1 } else { 0 };

        let footer_h = if self.input_mode {
            let inner_width = width.saturating_sub(4).max(1) as usize;
            let wrapped_len = wrap_input(&self.input_buffer, inner_width).len().max(1) as u16;
            wrapped_len + 2
        } else {
            1 + status_h
        };

        let chrome_h = 1 + 1 + footer_h;
        let body_h = term_h.saturating_sub(chrome_h);
        self.body_inner_height = body_h as usize;

        let mut y = area.y;

        let top_rect = Rect::new(area.x, y, width, 1);
        frame.render_widget(Paragraph::new(vec![top_rule]), top_rect);
        y += 1;

        if body_h > 0 {
            let body_rect = Rect::new(area.x, y, width, body_h);
            self.draw_list(frame, body_rect, degenerate);
            y += body_h;
        }

        let bottom_rect = Rect::new(area.x, y, width, 1);
        frame.render_widget(Paragraph::new(vec![bottom_rule]), bottom_rect);
        y += 1;

        if let Some(kind) = self.confirm_modal {
            self.draw_modal(frame, area, kind);
        } else if self.input_mode {
            let remain = area.height.saturating_sub(y - area.y);
            let input_rect = Rect::new(area.x, y, width, remain);
            self.draw_input(frame, input_rect);
        } else {
            let remain = area.height.saturating_sub(y - area.y);
            if remain > 0 {
                let footer_rect = Rect::new(area.x, y, width, remain);
                self.draw_footer(frame, footer_rect, degenerate);
            }
        }

        if self.palette.open && area.height > 0 && area.width > 0 && self.confirm_modal.is_none() {
            let overlay_h = self.palette_overlay_height(area.height);
            if overlay_h > 0 {
                let overlay = Rect::new(
                    area.x,
                    area.y + area.height.saturating_sub(overlay_h),
                    area.width,
                    overlay_h,
                );
                frame.render_widget(Clear, overlay);
                let lines = self.palette_lines(width, overlay_h);
                frame.render_widget(Paragraph::new(lines), overlay);
            }
        }
    }

    fn draw_list(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        degenerate: bool,
    ) {
        let visible = self.visible_entries();

        if visible.is_empty() {
            let message = Paragraph::new("No sessions yet — press n to create one")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(
                message,
                Rect::new(
                    area.x + area.width.saturating_sub(41) / 2,
                    area.y + area.height / 2,
                    area.width.min(41),
                    1,
                ),
            );
            return;
        }

        let now = SystemTime::now();
        let mut rendered_rows: Vec<(usize, Line<'static>)> = Vec::new();

        let mut selected_top_idx = 0;
        let mut selected_bottom_idx = 0;

        for (idx, entry) in visible.iter().enumerate() {
            let (badge, color, prefix) = phase_badge(entry.current_phase);
            let time = format_relative_time(entry.last_modified, now);

            let is_selected = idx == self.selected;
            let is_expanded = self.expanded.as_ref() == Some(&entry.session_id) && !degenerate;

            let leading = if is_selected { ">" } else { " " };
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
                Span::raw(entry.idea_summary.clone()),
            ]);
            let mut line = Line::from(spans);
            if is_selected {
                for span in &mut line.spans {
                    span.style = span.style.add_modifier(Modifier::BOLD);
                }
            }

            if is_selected {
                selected_top_idx = rendered_rows.len();
            }
            rendered_rows.push((idx, line));

            if is_expanded {
                let mut details = Vec::new();
                let dim = Style::default().fg(Color::DarkGray);
                let default_style = Style::default().fg(Color::White);

                let phase_status = if entry.current_phase == Phase::Done {
                    "done"
                } else if entry.current_phase == Phase::BlockedNeedsUser {
                    "blocked"
                } else if entry.current_phase == Phase::IdeaInput {
                    "awaiting input"
                } else {
                    "running"
                };

                details.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled("Phase: ", dim),
                    Span::styled(
                        format!("{} ({})", entry.current_phase.label(), phase_status),
                        default_style,
                    ),
                ]));

                let idea_text = if entry.idea_summary == "(no idea yet)" {
                    "(no idea yet)".to_string()
                } else {
                    let st = SessionState::load(&entry.session_id).ok();
                    st.and_then(|s| s.idea_text.clone())
                        .unwrap_or_else(|| entry.idea_summary.clone())
                };

                let wrap_width = area.width.saturating_sub(14).max(1) as usize;
                let idea_lines = wrap_input(&idea_text, wrap_width);
                for (i, il) in idea_lines.iter().enumerate() {
                    let prefix = if i == 0 { "Idea: " } else { "      " };
                    details.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled(prefix, dim),
                        Span::styled(il.clone(), default_style),
                    ]));
                }

                let st = SessionState::load(&entry.session_id).ok();
                let last_agent = st
                    .and_then(|s| s.agent_runs.last().map(|r| r.window_name.clone()))
                    .unwrap_or_else(|| "none".to_string());
                details.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled("Last agent: ", dim),
                    Span::styled(last_agent, default_style),
                ]));

                let modified: DateTime<Local> = entry.last_modified.into();
                details.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled("Modified: ", dim),
                    Span::styled(
                        modified.format("%Y-%m-%d %H:%M:%S").to_string(),
                        default_style,
                    ),
                ]));

                for d in details {
                    rendered_rows.push((idx, d));
                }
            }
            if is_selected {
                selected_bottom_idx = rendered_rows.len() - 1;
            }
        }

        if selected_top_idx < self.viewport_top {
            self.viewport_top = selected_top_idx;
        } else if selected_bottom_idx >= self.viewport_top + area.height as usize {
            self.viewport_top = selected_bottom_idx + 1 - area.height as usize;
        }

        let end = (self.viewport_top + area.height as usize).min(rendered_rows.len());
        let buf = frame.buffer_mut();
        for (i, (_, line)) in rendered_rows[self.viewport_top..end].iter().enumerate() {
            buf.set_line(area.x, area.y + i as u16, line, area.width);
        }
    }

    fn page_step(&self) -> usize {
        // Before the first draw there is no measured body height yet. The
        // interactive run loop always draws before reading input, so callers
        // without a render pass conservatively get a zero-step page move.
        self.body_inner_height.saturating_sub(1)
    }

    fn keymap_line(
        &self,
        width: u16,
        caps_fn: &dyn Fn(Option<Capability>) -> bool,
    ) -> Line<'static> {
        let nav = vec![
            KeyBinding {
                glyph: "↑↓",
                action: "move",
                is_primary: false,
                capability: None,
            },
            KeyBinding {
                glyph: "Space",
                action: "expand",
                is_primary: false,
                capability: Some(Capability::Expand),
            },
            KeyBinding {
                glyph: "PgUp/PgDn",
                action: "page",
                is_primary: false,
                capability: None,
            },
        ];
        let actions = vec![
            KeyBinding {
                glyph: "Enter",
                action: "open",
                is_primary: true,
                capability: Some(Capability::Input),
            },
            KeyBinding {
                glyph: "n",
                action: "new",
                is_primary: false,
                capability: None,
            },
        ];
        let system = vec![
            KeyBinding {
                glyph: ":",
                action: "palette",
                is_primary: false,
                capability: None,
            },
            KeyBinding {
                glyph: "Esc",
                action: "quit",
                is_primary: false,
                capability: None,
            },
        ];
        render_keymap_line(&[&nav, &actions, &system], caps_fn, width)
    }

    fn draw_footer(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        degenerate: bool,
    ) {
        let caps_fn = |cap: Option<Capability>| -> bool {
            match cap {
                Some(Capability::Expand) => !self.entries.is_empty(),
                Some(Capability::Input) => !self.entries.is_empty(),
                _ => true,
            }
        };

        let km_line = self.keymap_line(area.width, &caps_fn);
        let mut lines = Vec::new();

        if let Some(msg) = self.status_line.render() {
            if degenerate {
                lines.push(msg); // replace keymap with status
            } else {
                lines.push(msg);
                lines.push(km_line);
            }
        } else {
            lines.push(km_line);
        }

        let buf = frame.buffer_mut();
        for (i, line) in lines.iter().rev().enumerate() {
            let y = area.y + area.height.saturating_sub(1 + i as u16);
            if y >= area.y {
                buf.set_line(area.x, y, line, area.width);
            }
        }
    }

    fn draw_input(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let inner_width = area.width.saturating_sub(4).max(1) as usize;
        let placeholder = "describe your idea...";
        let (text, text_style) = if self.input_buffer.is_empty() {
            (
                placeholder.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            (self.input_buffer.clone(), Style::default().fg(Color::White))
        };

        let mut wrapped = wrap_input(&text, inner_width);
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }

        let cursor_pos = {
            let target = if self.input_buffer.is_empty() {
                0
            } else {
                self.input_cursor.min(self.input_buffer.chars().count())
            };
            let mut acc = 0usize;
            let mut found = (wrapped.len().saturating_sub(1), 0usize);
            for (idx, chunk) in wrapped.iter().enumerate() {
                let chunk_len = chunk.chars().count();
                if target <= acc + chunk_len {
                    found = (idx, target - acc);
                    break;
                }
                acc += chunk_len;
            }
            found
        };

        let mut content = Vec::new();
        for (idx, chunk) in wrapped.iter().enumerate() {
            let show_cursor_here = idx == cursor_pos.0;
            let split_col = if show_cursor_here { cursor_pos.1 } else { 0 };

            if show_cursor_here {
                let byte = chunk
                    .char_indices()
                    .nth(split_col)
                    .map(|(i, _)| i)
                    .unwrap_or(chunk.len());
                let (left, right) = (&chunk[..byte], &chunk[byte..]);
                content.push(Line::from(vec![
                    Span::styled(left.to_string(), text_style),
                    Span::styled(
                        "▌".to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ),
                    Span::styled(right.to_string(), text_style),
                ]));
            } else {
                content.push(Line::from(Span::styled(chunk.clone(), text_style)));
            }
        }

        let caps_fn: &dyn Fn(Option<Capability>) -> bool = &|_| true;
        let controls = vec![
            KeyBinding {
                glyph: "Enter",
                action: "create",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "Esc",
                action: "cancel",
                is_primary: false,
                capability: None,
            },
        ];
        let keymap = render_keymap_line(&[&controls], caps_fn, area.width);

        let sheet_lines = bottom_sheet(content, keymap, area.height, area.width);
        for (i, line) in sheet_lines.into_iter().enumerate() {
            if i < area.height as usize {
                frame
                    .buffer_mut()
                    .set_line(area.x, area.y + i as u16, &line, area.width);
            }
        }
    }

    fn draw_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        kind: ConfirmKind,
    ) {
        let entry = self
            .selected_entry()
            .map(|e| e.idea_summary.clone())
            .unwrap_or_default();
        let (title, warning) = match kind {
            ConfirmKind::Archive => ("Archive this session?", None),
            ConfirmKind::Delete => (
                "Permanently delete this session?",
                Some("This cannot be undone."),
            ),
        };

        let mut content = vec![Line::from(Span::styled(
            title,
            Style::default().fg(Color::White),
        ))];
        if let Some(w) = warning {
            content.push(Line::from(Span::styled(
                w,
                Style::default().fg(Color::White),
            )));
        }
        content.push(Line::from(""));
        content.push(Line::from(Span::styled(
            format!("\"{}\"", entry),
            Style::default().fg(Color::DarkGray),
        )));
        content.push(Line::from(""));

        let caps_fn: &dyn Fn(Option<Capability>) -> bool = &|_| true;
        let actions = vec![
            KeyBinding {
                glyph: "Enter",
                action: "confirm",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "Esc",
                action: "cancel",
                is_primary: false,
                capability: None,
            },
        ];
        let modal_keymap = render_keymap_line(
            &[&actions],
            caps_fn,
            area.width.clamp(40, 80).saturating_sub(4),
        );

        render_modal_overlay(frame, area, content, modal_keymap);
    }

    fn palette_inner_rows(&self) -> u16 {
        const MAX_OVERLAY_INNER: u16 = 12;
        let commands = self.palette_commands();
        let filtered = palette::filter(&self.palette.buffer, &commands);
        let suggestions = filtered.len().min(10) as u16;
        (1 + suggestions + 1).min(MAX_OVERLAY_INNER)
    }

    fn palette_overlay_height(&self, total_height: u16) -> u16 {
        const LIST_RESERVE: u16 = 4;
        let inner = self.palette_inner_rows();
        let desired = inner + 2;
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

        let inner_width = width.saturating_sub(2);

        let help = "Esc close  Tab complete  Enter run".to_string();
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
            KeyCode::Esc => Ok(KeyAction::Quit),
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
                            state.archived = true;
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
            KeyCode::Esc => {
                self.confirm_modal = None;
                Ok(KeyAction::Continue)
            }
            _ => Ok(KeyAction::Continue),
        }
    }

    fn palette_commands(&self) -> Vec<PaletteCommand> {
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
            state.archived = false;
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
            viewport_top: 0,
            body_inner_height: 0,
            expanded: None,
            input_mode: true,
            input_buffer: input_buffer.to_string(),
            input_cursor,
            show_archived: false,
            confirm_modal: None,
            create_modes: crate::state::Modes::default(),
            palette: PaletteState::default(),
            status_line: StatusLine::new(),
        }
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
            viewport_top: 0,
            body_inner_height: 0,
            expanded: None,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            show_archived: false,
            confirm_modal: None,
            create_modes: crate::state::Modes::default(),
            palette: PaletteState::default(),
            status_line: StatusLine::new(),
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
        let mut picker = picker_with_entries(
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

        // Borderless rows place the selection marker in the first cell.
        // Unselected rows keep that cell blank.
        let marker_cell = |y: u16| -> String { buf[(0, y)].symbol().to_string() };
        assert_eq!(marker_cell(alpha_y), " ", "unselected row stays blank");
        assert_eq!(marker_cell(beta_y), ">", "selected row shows > marker");

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
        let mut picker = picker_with_entries(vec![dummy_entry("alpha", "only idea")], 0);
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

    #[test]
    fn space_toggles_expansion_on_selected_session() {
        let mut picker = picker_with_entries(
            vec![
                dummy_entry("alpha", "first idea"),
                dummy_entry("beta", "second idea"),
            ],
            0,
        );

        assert_eq!(picker.expanded, None);

        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, Some("alpha".to_string()));

        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, None);
    }

    #[test]
    fn navigation_collapses_expanded_session() {
        let mut picker = picker_with_entries(
            vec![
                dummy_entry("alpha", "first idea"),
                dummy_entry("beta", "second idea"),
                dummy_entry("gamma", "third idea"),
            ],
            1,
        );

        // Expand beta
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, Some("beta".to_string()));

        // Move up collapses
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Up,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.expanded, None);

        // Expand alpha
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, Some("alpha".to_string()));

        // Move down collapses
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.expanded, None);
    }

    #[test]
    fn pgup_pgdn_use_visible_body_step_and_collapse_expansion() {
        let mut picker = picker_with_entries(
            (0..20)
                .map(|i| dummy_entry(&format!("sess-{i}"), &format!("idea {i}")))
                .collect(),
            8,
        );

        // term_h = 10 => body_h = 7, so PageUp/PageDown should move by 6
        // sessions rather than a fixed constant.
        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();

        // Expand sess-8.
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, Some("sess-8".to_string()));

        // PageUp collapses and moves by body_h - 1 sessions.
        picker
            .handle_key(KeyEvent::new(
                KeyCode::PageUp,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, None);
        assert_eq!(picker.selected, 2);

        // Re-expand and PageDown
        picker.selected = 8;
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, Some("sess-8".to_string()));

        picker
            .handle_key(KeyEvent::new(
                KeyCode::PageDown,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        assert_eq!(picker.expanded, None);
        assert_eq!(picker.selected, 14);
    }

    #[test]
    fn expanded_details_force_viewport_to_scroll_when_off_screen() {
        let entries: Vec<SessionEntry> = (0..8)
            .map(|i| dummy_entry(&format!("sess-{i}"), &format!("idea {i}")))
            .collect();
        let mut picker = picker_with_entries(entries, 0);

        // term_h = 10 => body_h ≈ 7 (10 - 1 - 1 - 1 footer), which fits 7 rows.
        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();
        assert_eq!(picker.viewport_top, 0);

        // Expand sess-0 (adds 4 detail rows).
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();

        // Now select sess-7. With sess-0 expanded we have 8 header + 4 detail = 12 rows.
        // body_h = 10 - 1 - 1 - 1 = 7 (no status line).
        // selected_top_idx for sess-7 = 7, selected_bottom_idx = 7.
        // 7 >= 0 + 7, so viewport_top should become 7 + 1 - 7 = 1.
        for _ in 0..7 {
            picker
                .handle_key(KeyEvent::new(
                    KeyCode::Down,
                    crossterm::event::KeyModifiers::NONE,
                ))
                .unwrap();
        }
        assert_eq!(picker.selected, 7);

        terminal.draw(|frame| picker.draw(frame)).unwrap();
        assert_eq!(
            picker.viewport_top, 1,
            "viewport_top should account for the expanded detail rows exactly"
        );
    }

    #[test]
    fn expanded_session_renders_detail_lines() {
        let entries = vec![dummy_entry("alpha", "only idea")];
        let mut picker = picker_with_entries(entries, 0);

        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        // Expand
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..12)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join(
                "
",
            );

        assert!(
            text.contains("Phase:"),
            "expanded session must show phase: {text}"
        );
        assert!(
            text.contains("Idea:"),
            "expanded session must show idea: {text}"
        );
        assert!(
            text.contains("Last agent:"),
            "expanded session must show last agent: {text}"
        );
        assert!(
            text.contains("Modified:"),
            "expanded session must show modified date: {text}"
        );
    }

    #[test]
    fn degenerate_terminal_omits_expansion() {
        let entries = vec![dummy_entry("alpha", "only idea")];
        let mut picker = picker_with_entries(entries, 0);
        picker.expanded = Some("alpha".to_string());

        // term_h < 10 triggers degenerate mode
        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..8)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join(
                "
",
            );

        assert!(
            !text.contains("Phase:"),
            "degenerate terminal must omit detail expansion: {text}"
        );
    }
}
