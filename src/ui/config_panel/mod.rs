#[cfg(test)]
use crate::data::config::{Config, mutate, schema::Override};
use crate::ui::chrome::{bottom_rule, top_rule_with_left_spans};
use crossterm::event::KeyEvent;
#[cfg(test)]
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::path::Path;
#[cfg(test)]
use std::{fs, path::PathBuf, time::SystemTime};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) mod providers;

const MIN_WIDTH: u16 = 50;
const TAB_SEPARATOR: &str = "  ";
const LABEL_WIDTH: usize = 28;
const TWO_PANE_MIN_WIDTH: usize = 100;
const TWO_PANE_MIN_DETAILS_WIDTH: usize = 34;

// Pipeline-style palette: focus accent matches the pipeline focus glyph,
// override accent picks up the warning yellow used for waiting nodes.
// Note: COLOR_DIM uses Gray (the lighter ANSI grey) rather than DarkGray
// because the config panel covers entire labels and values in dim text;
// DarkGray rendered as nearly-black on most themes and was hard to read.
const COLOR_FOCUS: Color = Color::Cyan;
const COLOR_OVERRIDE: Color = Color::Yellow;
const COLOR_DIM: Color = Color::Gray;
const COLOR_DANGER: Color = Color::Red;
const COLOR_OK: Color = Color::Green;
const COLOR_SECTION_TITLE: Color = Color::Magenta;
const COLOR_READONLY: Color = Color::Rgb(160, 160, 160);

pub(crate) use crate::app_runtime::views::config_panel::{
    ConfigPanelState, ConflictBanner, EXIT_OPTIONS, Editing, FIELDS, FieldKind, FieldMeta,
    PROVIDER_TOGGLES, PanelOutcome, ProviderToggle, SECTIONS, SectionLookup, ToggleField,
    is_providers_section, lookup_section, section_title, terminal_too_narrow_message,
};

impl ConfigPanelState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        let ui_key = crate::ui::tui::ui_key_from_event(key);
        // In editing or search mode most character keys must be treated as
        // literal text input, not as the navigation shortcuts they are when
        // the panel is in navigation mode.
        let cmd = if self.editing.is_some() || self.searching.is_some() {
            match ui_key.code {
                crate::app::keys::UiKeyCode::Esc => {
                    crate::app_runtime::commands::ConfigPanelCommand::Close
                }
                crate::app::keys::UiKeyCode::Enter => {
                    crate::app_runtime::commands::ConfigPanelCommand::Activate
                }
                crate::app::keys::UiKeyCode::Up => {
                    crate::app_runtime::commands::ConfigPanelCommand::MoveUp
                }
                crate::app::keys::UiKeyCode::Down => {
                    crate::app_runtime::commands::ConfigPanelCommand::MoveDown
                }
                crate::app::keys::UiKeyCode::Backspace => {
                    crate::app_runtime::commands::ConfigPanelCommand::Edit(
                        crate::app_runtime::commands::InputCommand::Backspace,
                    )
                }
                crate::app::keys::UiKeyCode::Char(c) if !ui_key.ctrl && !ui_key.alt => {
                    crate::app_runtime::commands::ConfigPanelCommand::Edit(
                        crate::app_runtime::commands::InputCommand::InsertText(c.to_string()),
                    )
                }
                _ => crate::app::config_panel::config_panel_key_to_command(ui_key),
            }
        } else {
            crate::app::config_panel::config_panel_key_to_command(ui_key)
        };
        self.handle_command(cmd)
    }
}

pub(crate) fn render(frame: &mut Frame<'_>, area: Rect, state: &ConfigPanelState) {
    frame.render_widget(ConfigPanelWidget { state }, area);
}

struct ConfigPanelWidget<'a> {
    state: &'a ConfigPanelState,
}

impl Widget for ConfigPanelWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        if area.width < MIN_WIDTH {
            Paragraph::new(terminal_too_narrow_message()).render(area, buf);
            return;
        }
        let mut lines = adaptive_lines(self.state, area.width, area.height);
        lines.truncate(area.height as usize);
        while lines.len() < area.height as usize {
            lines.push(Line::from(""));
        }
        Paragraph::new(lines).render(area, buf);
        render_dropdown(self.state, area, buf);
        render_search_overlay(self.state, area, buf);
        render_add_provider_overlay(self.state, area, buf);
        render_provider_detail_overlay(self.state, area, buf);
        render_exit_prompt_overlay(self.state, area, buf);
    }
}

// --- helpers shared by all renderers ----------------------------------------

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(COLOR_DIM))
}

fn overlay_keymap_line(bindings: &[(&str, &str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (i, (key, action)) in bindings.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        // First binding in list is primary action - use cyan
        let key_color = if i == 0 { COLOR_FOCUS } else { Color::White };
        let key_style = if i == 0 {
            Style::default().fg(key_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(key_color)
        };
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            action.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

fn focus_span(focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            "▌".to_string(),
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    }
}

fn override_dot(is_override: bool) -> Span<'static> {
    if is_override {
        Span::styled("●".to_string(), Style::default().fg(COLOR_OVERRIDE))
    } else {
        Span::raw(" ")
    }
}

pub(crate) fn pad_right(text: &str, width: usize) -> String {
    let used = text.width();
    if used >= width {
        ellipsize_end(text, width)
    } else {
        format!("{text}{}", " ".repeat(width - used))
    }
}

fn render_add_provider_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(Editing::AddProvider(editor)) = state.editing.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }

    let overlay_w = area.width.saturating_sub(10).max(54).min(area.width);
    let overlay_h: u16 = 14;
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let label_w: usize = 12;
    let value_for = |field: providers::AddProviderField| -> String {
        match field {
            providers::AddProviderField::Model => editor.model.clone(),
            providers::AddProviderField::Subscription => editor.subscription.clone(),
            providers::AddProviderField::Cli => editor.cli.as_str().to_string(),
            providers::AddProviderField::Official => {
                if editor.official {
                    "✓ on".to_string()
                } else {
                    "✗ off".to_string()
                }
            }
            providers::AddProviderField::Free => {
                if editor.free {
                    "✓ on".to_string()
                } else {
                    "✗ off".to_string()
                }
            }
            providers::AddProviderField::LaunchName => format!("{}_", editor.launch_name),
        }
    };
    let render_row = |field: providers::AddProviderField, label: &str| -> Line<'static> {
        let focused = editor.focus == field;
        let value_style = if focused {
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans: Vec<Span<'static>> = vec![
            Span::raw(" "),
            focus_span(focused),
            Span::raw(" "),
            Span::styled(pad_right(label, label_w), Style::default().fg(COLOR_DIM)),
            Span::raw(" "),
            Span::styled(value_for(field), value_style),
        ];
        // Show a chevron next to enum-style fields so the dropdown
        // affordance reads at a glance.
        if matches!(
            field,
            providers::AddProviderField::Model
                | providers::AddProviderField::Subscription
                | providers::AddProviderField::Cli
        ) {
            spans.push(Span::styled(
                " ▼".to_string(),
                Style::default().fg(COLOR_DIM),
            ));
        }
        if focused {
            let hint = match field {
                providers::AddProviderField::LaunchName => "  type · Enter to add",
                providers::AddProviderField::Official | providers::AddProviderField::Free => {
                    "  Space to toggle"
                }
                _ => "  Enter to choose",
            };
            spans.push(Span::styled(
                hint.to_string(),
                Style::default().fg(COLOR_DIM),
            ));
        }
        Line::from(spans)
    };

    let lines: Vec<Line<'static>> = vec![
        Line::from(""),
        render_row(providers::AddProviderField::Model, "Model"),
        render_row(providers::AddProviderField::Subscription, "Subscription"),
        render_row(providers::AddProviderField::Cli, "CLI"),
        render_row(providers::AddProviderField::Official, "Official"),
        render_row(providers::AddProviderField::Free, "Free"),
        render_row(providers::AddProviderField::LaunchName, "Launch name"),
        Line::from(""),
        overlay_keymap_line(&[
            ("Enter", "choose/commit"),
            ("Space", "toggle"),
            ("Tab", "field"),
            ("Esc", "cancel"),
        ]),
    ];

    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS))
                .title(Span::styled(
                    " New model ".to_string(),
                    Style::default()
                        .fg(COLOR_FOCUS)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);

    // Dropdown popup overlay on top of the modal when an enum field is open.
    if let Some(target) = editor.open_dropdown {
        render_add_provider_dropdown(editor, target, rect, area, buf);
    }
}

fn render_add_provider_dropdown(
    editor: &providers::ProvidersEditor,
    target: providers::AddProviderField,
    modal_rect: Rect,
    area: Rect,
    buf: &mut Buffer,
) {
    let options = editor.dropdown_options(target);
    if options.is_empty() {
        return;
    }
    let label_w: usize = 12;
    let max_opt_w = options.iter().map(|o| o.width()).max().unwrap_or(0);
    let popup_w = ((max_opt_w + 4) as u16).max(20).min(area.width);
    let popup_h_wanted = (options.len() as u16 + 2).min(12); // border + max 10 rows
    let popup_h = popup_h_wanted.min(area.height.saturating_sub(2).max(3));
    if popup_h < 3 {
        return;
    }

    // Anchor under the field row inside the modal (modal_rect.y is the top
    // border; rows 1..=4 are the four form fields).
    let field_row_offset: u16 = match target {
        providers::AddProviderField::Model => 2,
        providers::AddProviderField::Subscription => 3,
        providers::AddProviderField::Cli => 4,
        _ => 2,
    };
    let row_y = modal_rect.y.saturating_add(field_row_offset);
    let popup_x = modal_rect
        .x
        .saturating_add(1 + 1 + 1 + label_w as u16)
        .min(area.x + area.width.saturating_sub(popup_w));
    let area_bottom = area.y.saturating_add(area.height);
    let mut popup_y = row_y.saturating_add(1);
    if popup_y + popup_h > area_bottom {
        popup_y = row_y.saturating_sub(popup_h);
    }
    let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
    Clear.render(popup_rect, buf);

    let inner_rows = popup_h.saturating_sub(2) as usize;
    let cursor = editor.dropdown_cursor.min(options.len().saturating_sub(1));
    let win_start = cursor
        .saturating_sub(inner_rows.saturating_sub(1))
        .min(options.len().saturating_sub(inner_rows.min(options.len())));
    let lines: Vec<Line<'static>> = options
        .iter()
        .enumerate()
        .skip(win_start)
        .take(inner_rows)
        .map(|(i, opt)| {
            let focused = i == cursor;
            Line::from(vec![
                focus_span(focused),
                Span::raw(" "),
                Span::styled(
                    opt.clone(),
                    if focused {
                        Style::default()
                            .fg(COLOR_FOCUS)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
            ])
        })
        .collect();
    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS)),
        )
        .render(popup_rect, buf);
}

fn render_provider_detail_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(Editing::ProviderDetail { cursor }) = state.editing.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }
    let cursor = *cursor;
    let lines = providers::get_lines(&state.config, &state.folded_vendors);
    let Some(providers::ProvidersLine::Provider { entry, is_baked }) =
        lines.get(state.providers_cursor)
    else {
        return;
    };

    let subscription_label =
        crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription)
            .to_string();
    let title = format!(" {} · {} ", subscription_label, entry.model,);
    let source = if *is_baked { "built-in" } else { "custom" };

    let row_count = PROVIDER_TOGGLES.len();
    let overlay_h = (row_count as u16) + 6; // border + title row + chrome
    let overlay_h = overlay_h.min(area.height);
    let overlay_w = area.width.saturating_sub(10).max(56).min(area.width);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let mut body: Vec<Line<'static>> = Vec::new();
    body.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{} · {}", entry.cli.as_str(), entry.launch_name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("({source})"), Style::default().fg(COLOR_DIM)),
    ]));
    body.push(Line::from(""));

    for (idx, toggle) in PROVIDER_TOGGLES.iter().enumerate() {
        let on = current_toggle_value(entry, toggle);
        let focused = idx == cursor;
        let locked = *is_baked && toggle.baked_locked;
        let check_glyph = if on { " ✓ " } else { " ✗ " };
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw(" "));
        spans.push(focus_span(focused));
        spans.push(Span::raw(" "));
        let check_style = if locked {
            Style::default().fg(COLOR_READONLY)
        } else if on {
            Style::default().fg(COLOR_OK).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_DIM)
        };
        spans.push(Span::styled(check_glyph.to_string(), check_style));
        spans.push(Span::raw(" "));
        let label_style = if focused {
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD)
        } else if locked {
            Style::default().fg(COLOR_READONLY)
        } else {
            Style::default()
        };
        spans.push(Span::styled(toggle.label.to_string(), label_style));
        if locked {
            spans.push(Span::styled(
                "  built-in".to_string(),
                Style::default().fg(COLOR_READONLY),
            ));
        }
        body.push(Line::from(spans));
    }

    body.push(Line::from(""));
    let active_desc = PROVIDER_TOGGLES
        .get(cursor)
        .map(|t| t.description)
        .unwrap_or_default();
    body.push(Line::from(dim(format!(" {active_desc}"))));
    body.push(overlay_keymap_line(&[
        ("↑↓", "navigate"),
        ("Space", "toggle"),
        ("x", "delete"),
        ("Esc", "close"),
    ]));

    Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS))
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(COLOR_FOCUS)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);
}

fn render_exit_prompt_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(ConflictBanner::ExitPrompt { selected }) = state.conflict.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }
    let selected = *selected;

    let overlay_w = area.width.saturating_sub(10).max(56).min(area.width);
    let overlay_h: u16 = (EXIT_OPTIONS.len() as u16) + 6; // border + title + change line + spacing + footer
    let overlay_h = overlay_h.min(area.height);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let mut body: Vec<Line<'static>> = Vec::new();
    body.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Your changes haven't been written to disk yet.".to_string(),
            Style::default().fg(COLOR_OVERRIDE),
        ),
    ]));
    body.push(Line::from(""));

    for (idx, (_, label, key, hint)) in EXIT_OPTIONS.iter().enumerate() {
        let focused = idx == selected;
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw(" "));
        spans.push(focus_span(focused));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("[{key}]"),
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        let label_style = if focused {
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label.to_string(), label_style));
        if focused {
            spans.push(Span::styled(
                format!("  · {hint}"),
                Style::default().fg(COLOR_DIM),
            ));
        }
        body.push(Line::from(spans));
    }

    body.push(Line::from(""));
    body.push(overlay_keymap_line(&[
        ("↑↓", "select"),
        ("Enter", "confirm"),
        ("s/d/c", "shortcut"),
        ("Esc", "cancel"),
    ]));

    Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_OVERRIDE))
                .title(Span::styled(
                    " Unsaved changes ".to_string(),
                    Style::default()
                        .fg(COLOR_OVERRIDE)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);
}

fn current_toggle_value(
    entry: &crate::data::config::schema::ProviderEntry,
    toggle: &ProviderToggle,
) -> bool {
    match toggle.field {
        ToggleField::Enabled => entry.enabled,
        ToggleField::Official => entry.official,
        ToggleField::Free => entry.free,
        ToggleField::QuotaDisabled => entry.quota_disabled,
        ToggleField::Cheap => entry.cheap_eligible,
        ToggleField::Tough => entry.tough_eligible,
        ToggleField::Effort => entry.effort_eligible,
    }
}

fn render_search_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(search) = state.searching.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }

    let overlay_w = area.width.saturating_sub(4).max(30).min(area.width);
    let max_results: u16 = 8;
    let overlay_h = (search.results.len() as u16).min(max_results) + 4;
    let overlay_h = overlay_h.min(area.height);
    if overlay_h < 5 {
        return;
    }
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let inner_w = overlay_w.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(
            "/ ".to_string(),
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{}_", search.query)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner_w),
        Style::default().fg(COLOR_DIM),
    )));

    if search.results.is_empty() {
        lines.push(Line::from(dim("  no fields match")));
    } else {
        let max_rows = (overlay_h.saturating_sub(4)) as usize;
        let total = search.results.len();
        let win_start = search
            .selected
            .saturating_sub(max_rows.saturating_sub(1))
            .min(total.saturating_sub(max_rows.min(total)));
        for (offset, field_idx) in search
            .results
            .iter()
            .enumerate()
            .skip(win_start)
            .take(max_rows)
        {
            let meta = FIELDS[*field_idx];
            let focused = offset == search.selected;
            lines.push(Line::from(vec![
                focus_span(focused),
                Span::raw(" "),
                Span::styled(
                    meta.label.to_string(),
                    if focused {
                        Style::default()
                            .fg(COLOR_FOCUS)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled("  ".to_string(), Style::default().fg(COLOR_DIM)),
                Span::styled(meta.key.to_string(), Style::default().fg(COLOR_DIM)),
            ]));
        }
    }
    lines.push(overlay_keymap_line(&[
        ("↑↓", "select"),
        ("Enter", "jump"),
        ("Esc", "cancel"),
    ]));

    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS))
                .title(Span::styled(
                    " Search ".to_string(),
                    Style::default()
                        .fg(COLOR_FOCUS)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);
}

fn render_dropdown(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(Editing::Choice {
        options, selected, ..
    }) = state.editing.as_ref()
    else {
        return;
    };
    if options.is_empty() {
        return;
    }
    let w = area.width as usize;
    let tab_lines = tab_bar_lines(state, w).len() as u16;
    // Body region: header(1) + tab_lines + separator(1) starts the rows;
    // bottom 3 rows are reserved for separator + help + footer.
    let body_y_start = area.y.saturating_add(2 + tab_lines);
    let body_y_end = area.y.saturating_add(area.height).saturating_sub(3);
    if body_y_end <= body_y_start {
        return;
    }

    let visible = visible_fields(state);
    let pos = visible
        .iter()
        .position(|i| *i == state.selected_field)
        .unwrap_or(0);
    let row_y = body_y_start.saturating_add(pos as u16);
    if row_y >= body_y_end {
        return;
    }

    let max_opt_w = options.iter().map(|o| o.width()).max().unwrap_or(1);
    let popup_w = ((max_opt_w + 4) as u16).max(10).min(area.width);
    let popup_h_wanted = options.len() as u16 + 2; // top/bottom border
    let avail_h = body_y_end.saturating_sub(body_y_start);
    let popup_h = popup_h_wanted.min(avail_h);
    if popup_h < 3 {
        return;
    }

    let name_w = w.min(30) as u16;
    let mut popup_x = area.x.saturating_add(name_w + 3);
    let area_right = area.x.saturating_add(area.width);
    if popup_x + popup_w > area_right {
        popup_x = area_right.saturating_sub(popup_w);
    }
    if popup_x < area.x {
        popup_x = area.x;
    }

    // Anchor below the focused row; if there is not enough room, place it
    // above. Falling back to the bottom of the body keeps the popup
    // fully visible when nothing else fits.
    let mut popup_y = row_y.saturating_add(1);
    if popup_y + popup_h > body_y_end {
        let above_room = row_y.saturating_sub(body_y_start);
        if above_room >= popup_h {
            popup_y = row_y.saturating_sub(popup_h);
        } else {
            popup_y = body_y_end.saturating_sub(popup_h);
        }
    }

    let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
    Clear.render(popup_rect, buf);

    let lines: Vec<Line<'static>> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let focused = i == *selected;
            Line::from(vec![
                focus_span(focused),
                Span::raw(" "),
                Span::styled(
                    opt.clone(),
                    if focused {
                        Style::default()
                            .fg(COLOR_FOCUS)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
            ])
        })
        .collect();

    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS)),
        )
        .render(popup_rect, buf);
}

fn adaptive_lines(state: &ConfigPanelState, width: u16, height: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    let h = height as usize;
    let mut lines = Vec::new();

    // Header with title
    lines.push(header_line(&state.path, width));

    // Tab bar with section navigation
    let tab_lines = tab_bar_lines(state, w);
    lines.extend(tab_lines);

    // Separator after tabs
    lines.push(bottom_rule(width, None));

    // Calculate body height: reserve space for footer chrome
    // Footer chrome = separator (1) + help (1) + keymap (1) = 3 lines
    let chrome_used = lines.len() + 3;
    let body_h = h.saturating_sub(chrome_used);

    let use_two_pane =
        body_h >= 4 && w >= TWO_PANE_MIN_WIDTH && !is_providers_section(state.current_section());

    if use_two_pane {
        let sep = " │ ";
        let sep_w = sep.width();
        let left_w = w
            .saturating_sub(TWO_PANE_MIN_DETAILS_WIDTH + sep_w)
            .max(MIN_WIDTH as usize)
            .min(w.saturating_sub(sep_w + TWO_PANE_MIN_DETAILS_WIDTH));
        let right_w = w.saturating_sub(left_w + sep_w).max(1);

        // Left pane: section header + rows (padded to `body_h`).
        let mut left_lines: Vec<Line<'static>> = Vec::with_capacity(body_h);
        left_lines.push(section_header_line(state, left_w));
        let content_h = body_h.saturating_sub(1);
        let fields = visible_fields(state);
        for idx in fields.into_iter().take(content_h) {
            left_lines.push(field_row(state, idx, left_w));
        }
        while left_lines.len() < body_h {
            left_lines.push(Line::from(""));
        }

        // Right pane: details card, same height as left body.
        let mut right_lines = details_panel_lines(state, right_w, body_h);
        right_lines.truncate(body_h);
        while right_lines.len() < body_h {
            right_lines.push(Line::from(""));
        }

        // Merge panes into full-width lines.
        for (left, right) in left_lines.into_iter().zip(right_lines) {
            lines.push(merge_two_pane_line(left, right, left_w, right_w, sep));
        }
    } else {
        // Section title with visual treatment
        if body_h > 0 {
            lines.push(section_header_line(state, w));
        }

        // Adjust body_h for section header
        let content_h = body_h.saturating_sub(1);

        // Body content
        if is_providers_section(state.current_section()) {
            for body_line in providers_body_lines(state, w, content_h)
                .into_iter()
                .take(content_h)
            {
                lines.push(body_line);
            }
        } else {
            let fields = visible_fields(state);
            for idx in fields.into_iter().take(content_h) {
                lines.push(field_row(state, idx, w));
            }
        }
    }

    // Pad to fill remaining body space for consistent footer positioning
    let target_body_end = chrome_used.saturating_sub(3) + body_h;
    while lines.len() < target_body_end {
        lines.push(Line::from(""));
    }

    // Footer chrome: separator + help + keymap (always at bottom)
    lines.push(bottom_rule(width, None));
    lines.push(help_text(state, w));
    lines.push(footer_line(state, w));

    // Ensure we don't exceed height
    lines.truncate(h);
    lines
}

fn section_header_line(state: &ConfigPanelState, width: usize) -> Line<'static> {
    let section = state.current_section();
    let title = section_title(section);
    let override_count = state.section_override_count(section);

    let mut spans: Vec<Span<'static>> = Vec::new();

    // Section icon based on type
    let icon = match section {
        "general" => "⚙",
        "models" => "◇",
        "notifications" => "🔔",
        "agents" => "⚡",
        "system" => "⊕",
        _ => "▸",
    };

    spans.push(Span::styled(
        format!(" {icon} "),
        Style::default().fg(COLOR_SECTION_TITLE),
    ));
    spans.push(Span::styled(
        title.to_string(),
        Style::default()
            .fg(COLOR_SECTION_TITLE)
            .add_modifier(Modifier::BOLD),
    ));

    if override_count > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(
                "({} override{})",
                override_count,
                if override_count == 1 { "" } else { "s" }
            ),
            Style::default().fg(COLOR_OVERRIDE),
        ));
    }

    // Fill remaining width with subtle separator
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let remaining = width.saturating_sub(used);
    if remaining > 0 {
        spans.push(Span::styled(" ".repeat(remaining), Style::default()));
    }

    Line::from(spans)
}

fn details_panel_lines(
    state: &ConfigPanelState,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if width < 10 || height == 0 {
        return vec![Line::from("")];
    }

    let inner_w = width.saturating_sub(2);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height);

    let title = "Details";
    let label = format!(" {title} ");
    let top = if inner_w > label.width() + 2 {
        let remaining = inner_w.saturating_sub(label.width());
        let left = remaining / 2;
        let right = remaining.saturating_sub(left);
        format!("┌{}{}{}┐", "─".repeat(left), label, "─".repeat(right))
    } else {
        format!("┌{}┐", "─".repeat(inner_w))
    };
    out.push(Line::from(Span::styled(
        ellipsize_end(&top, width),
        Style::default().fg(COLOR_DIM),
    )));

    let Some(meta) = state.current_meta() else {
        for _ in 1..height.saturating_sub(1) {
            out.push(Line::from(Span::raw(format!("│{}│", " ".repeat(inner_w)))));
        }
        let bottom = format!("└{}┘", "─".repeat(inner_w));
        out.push(Line::from(Span::styled(
            bottom,
            Style::default().fg(COLOR_DIM),
        )));
        return out;
    };

    let source = state.source_for(meta);
    let source_label = match source {
        "override" => "Override",
        "(def)" | "default" => "Default",
        other => other,
    };
    let key_line = format!("Setting: {}", meta.label);
    let source_line = format!("Source: {source_label}");

    let mut value = if matches!(meta.kind, FieldKind::Bool) {
        render_value_text(state, meta, false)
    } else {
        state.value_for(meta)
    };
    if meta.secret && !state.reveal_topic && !value.is_empty() {
        value = middle_ellipsis(&value, 24);
    }
    let value_line = if value.is_empty() {
        "Value: (empty)".to_string()
    } else {
        format!("Value: {value}")
    };

    let mut body: Vec<String> = Vec::new();
    body.push(key_line);
    body.push(source_line);
    body.push(String::new());
    body.extend(crate::ui::tui::wrap_text(&value_line, inner_w));
    body.push(String::new());
    body.extend(crate::ui::tui::wrap_text(meta.description, inner_w));

    let max_body_lines = height.saturating_sub(2);
    for i in 0..max_body_lines {
        let content = body.get(i).cloned().unwrap_or_default();
        let clipped = ellipsize_end(&content, inner_w);
        let padded = pad_right(&clipped, inner_w);
        out.push(Line::from(vec![
            Span::styled("│".to_string(), Style::default().fg(COLOR_DIM)),
            Span::raw(padded),
            Span::styled("│".to_string(), Style::default().fg(COLOR_DIM)),
        ]));
    }

    let bottom = format!("└{}┘", "─".repeat(inner_w));
    out.push(Line::from(Span::styled(
        bottom,
        Style::default().fg(COLOR_DIM),
    )));
    out
}

fn merge_two_pane_line(
    left: Line<'static>,
    right: Line<'static>,
    left_w: usize,
    right_w: usize,
    sep: &str,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left = left;
    let left_used = left.width();
    if left_used < left_w {
        left.spans.push(Span::raw(" ".repeat(left_w - left_used)));
    }
    spans.extend(left.spans);
    spans.push(Span::styled(
        sep.to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    let mut right = right;
    let right_used = right.width();
    if right_used < right_w {
        right
            .spans
            .push(Span::raw(" ".repeat(right_w - right_used)));
    }
    spans.extend(right.spans);
    Line::from(spans)
}

fn tab_bar_lines(state: &ConfigPanelState, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_w: usize = 0;
    for (i, section) in SECTIONS.iter().enumerate() {
        let active = i == state.selected_section;
        let dirty = state.section_override_count(section) > 0;
        let title = section_title(section);
        // Account for brackets around active tab
        let bracket_extra = if active { 2 } else { 0 };
        let plain_w = title.width() + bracket_extra + if dirty { 2 } else { 0 };
        let sep_w = if current_spans.is_empty() {
            1
        } else {
            TAB_SEPARATOR.width()
        };
        if !current_spans.is_empty() && current_w + sep_w + plain_w > width {
            lines.push(Line::from(std::mem::take(&mut current_spans)));
            current_w = 0;
        }
        if current_spans.is_empty() {
            current_spans.push(Span::raw(" "));
            current_w += 1;
        } else {
            current_spans.push(Span::styled(
                TAB_SEPARATOR.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            current_w += TAB_SEPARATOR.width();
        }
        // Active tab gets bracket treatment for better visibility
        if active {
            current_spans.push(Span::styled(
                "[".to_string(),
                Style::default().fg(COLOR_FOCUS),
            ));
        }
        let title_style = if active {
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_DIM)
        };
        current_spans.push(Span::styled(title.to_string(), title_style));
        if active {
            current_spans.push(Span::styled(
                "]".to_string(),
                Style::default().fg(COLOR_FOCUS),
            ));
        }
        if dirty {
            current_spans.push(Span::raw(" "));
            current_spans.push(Span::styled(
                "●".to_string(),
                Style::default().fg(COLOR_OVERRIDE),
            ));
        }
        current_w += plain_w;
    }
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn field_row(state: &ConfigPanelState, idx: usize, width: usize) -> Line<'static> {
    let meta = &FIELDS[idx];
    let focused = idx == state.selected_field;
    let is_override = state.source_for(meta) == "override";
    let field_locked = matches!(
        meta.kind,
        FieldKind::ReadOnly | FieldKind::List | FieldKind::Map
    );

    let value_text = if focused && matches!(state.editing, Some(Editing::Integer | Editing::String))
    {
        state.edit_buffer.clone()
    } else {
        render_value_text(state, meta, focused)
    };

    // Width budget: 1 (focus) + 1 (override) + 1 (space) + LABEL_WIDTH +
    // 1 (space) + 1 (separator) + 1 (space) + value (rest) + maybe chip.
    let label_w = LABEL_WIDTH.min(width.saturating_sub(8));
    let prefix_w = 1 + 1 + 1 + label_w + 1 + 1 + 1; // = label_w + 6
    let chip_text = if is_override { " (overridden)" } else { "" };
    let chip_w = chip_text.width();
    let value_w = width.saturating_sub(prefix_w + chip_w).max(1);
    let value_clipped = ellipsize_end(&value_text, value_w);

    // Locked fields render in light gray to signal "read-only" at a glance.
    let label_style = if field_locked {
        Style::default().fg(COLOR_READONLY)
    } else if focused {
        Style::default()
            .fg(COLOR_FOCUS)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let value_style = if field_locked {
        Style::default().fg(COLOR_READONLY)
    } else if focused && matches!(state.editing, Some(Editing::Integer | Editing::String)) {
        Style::default()
            .fg(COLOR_FOCUS)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else if is_override {
        Style::default().fg(COLOR_OVERRIDE)
    } else {
        Style::default()
    };

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    spans.push(focus_span(focused));
    spans.push(override_dot(is_override));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(pad_right(meta.label, label_w), label_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        "│".to_string(),
        Style::default().fg(COLOR_DIM),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(value_clipped, value_style));

    if focused
        && matches!(meta.kind, FieldKind::Enum(_))
        && !matches!(state.editing, Some(Editing::Choice { .. }))
    {
        spans.push(Span::styled(
            " ▼".to_string(),
            Style::default().fg(COLOR_DIM),
        ));
    }

    if is_override {
        spans.push(Span::styled(
            chip_text.to_string(),
            Style::default().fg(COLOR_OVERRIDE),
        ));
    }

    Line::from(spans)
}

fn render_value_text(state: &ConfigPanelState, meta: &FieldMeta, _focused: bool) -> String {
    let raw = state.value_for(meta);
    if meta.secret && !state.reveal_topic && !raw.is_empty() {
        middle_ellipsis(&raw, 16)
    } else if matches!(meta.kind, FieldKind::Bool) {
        if raw == "true" {
            "✓ on".to_string()
        } else {
            "✗ off".to_string()
        }
    } else {
        raw
    }
}

fn help_text(state: &ConfigPanelState, width: usize) -> Line<'static> {
    if let Some(err) = state
        .current_meta()
        .and_then(|meta| state.edit_error_for(*meta, &state.edit_buffer))
    {
        return Line::from(vec![
            Span::styled("✗ ".to_string(), Style::default().fg(COLOR_DANGER)),
            Span::styled(
                ellipsize_end(&err, width.saturating_sub(2)),
                Style::default().fg(COLOR_DANGER),
            ),
        ]);
    }
    if let Some(err) = &state.save_error {
        return Line::from(vec![
            Span::styled("✗ ".to_string(), Style::default().fg(COLOR_DANGER)),
            Span::styled(
                ellipsize_end(err, width.saturating_sub(2)),
                Style::default().fg(COLOR_DANGER),
            ),
        ]);
    }
    let banner = match &state.conflict {
        Some(ConflictBanner::MtimeAdvanced) => {
            Some("⚠ mtime conflict: r reload · o overwrite · Esc keep editing")
        }
        Some(ConflictBanner::ExitPrompt { .. }) => None,
        Some(ConflictBanner::RegenerateTopicPrompt) => {
            Some("⚠ regenerate ntfy.topic? y accept · n keep")
        }
        None => None,
    };
    if let Some(text) = banner {
        return Line::from(Span::styled(
            ellipsize_end(text, width),
            Style::default().fg(COLOR_OVERRIDE),
        ));
    }

    let description = if is_providers_section(state.current_section()) {
        let lines = providers::get_lines(&state.config, &state.folded_vendors);
        match lines.get(state.providers_cursor) {
            Some(providers::ProvidersLine::VendorHeader { folded: true, .. }) => {
                "Space expands · n new model".to_string()
            }
            Some(providers::ProvidersLine::VendorHeader { folded: false, .. }) => {
                "Space folds · n new model".to_string()
            }
            Some(providers::ProvidersLine::Provider { .. }) => {
                "Enter details · x remove".to_string()
            }
            Some(providers::ProvidersLine::AddAction) => "Enter to create".to_string(),
            _ => "Space toggle · n new model".to_string(),
        }
    } else {
        state
            .current_meta()
            .map(|meta| meta.description.to_string())
            .unwrap_or_default()
    };

    let invalid = state.current_validation_error();
    let (status_icon, status_text, status_color) = if let Some(reason) = invalid {
        ("✗ ", reason, COLOR_DANGER)
    } else if state.dirty {
        (
            "● ",
            format!(
                "{} change{} · ^S to save",
                dirty_count(state),
                if dirty_count(state) == 1 { "" } else { "s" }
            ),
            COLOR_OVERRIDE,
        )
    } else {
        ("", state.status.clone(), COLOR_DIM)
    };

    let sep = " │ ";
    let icon_w = status_icon.width();
    let status_w = status_text.width() + icon_w;
    let sep_w = sep.width();
    let desc_budget = width.saturating_sub(sep_w + status_w);

    if desc_budget >= 12 && !description.is_empty() {
        let desc_clipped = ellipsize_end(&description, desc_budget);
        let desc_w = desc_clipped.width();
        let fill = desc_budget.saturating_sub(desc_w);
        let mut spans = vec![dim(desc_clipped)];
        if fill > 0 {
            spans.push(Span::raw(" ".repeat(fill)));
        }
        spans.push(Span::styled(
            sep.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
        if !status_icon.is_empty() {
            spans.push(Span::styled(
                status_icon.to_string(),
                Style::default().fg(status_color),
            ));
        }
        spans.push(Span::styled(
            ellipsize_end(
                &status_text,
                width.saturating_sub(desc_budget + sep_w + icon_w).max(1),
            ),
            Style::default().fg(status_color),
        ));
        Line::from(spans)
    } else {
        let mut spans = Vec::new();
        if !status_icon.is_empty() {
            spans.push(Span::styled(
                status_icon.to_string(),
                Style::default().fg(status_color),
            ));
        }
        spans.push(Span::styled(
            ellipsize_end(&status_text, width.saturating_sub(icon_w)),
            Style::default().fg(status_color),
        ));
        Line::from(spans)
    }
}

struct ConfigKey {
    glyph: &'static str,
    action: &'static str,
    primary: bool,
}

const fn ck(glyph: &'static str, action: &'static str) -> ConfigKey {
    ConfigKey {
        glyph,
        action,
        primary: false,
    }
}

const fn ck_primary(glyph: &'static str, action: &'static str) -> ConfigKey {
    ConfigKey {
        glyph,
        action,
        primary: true,
    }
}

fn footer_bindings(state: &ConfigPanelState) -> (Vec<ConfigKey>, Vec<ConfigKey>) {
    if state.searching.is_some() {
        return (
            vec![ck("↑↓", "select"), ck_primary("Enter", "jump")],
            vec![ck("Esc", "cancel")],
        );
    }
    let in_dropdown = matches!(
        &state.editing,
        Some(Editing::AddProvider(editor)) if editor.open_dropdown.is_some()
    );
    if in_dropdown {
        return (
            vec![ck("↑↓", "select"), ck_primary("Enter", "pick")],
            vec![ck("Esc", "cancel")],
        );
    }
    match &state.editing {
        Some(Editing::Choice { .. }) => (
            vec![ck("↑↓", "select"), ck_primary("Enter", "commit")],
            vec![ck("Esc", "cancel")],
        ),
        Some(Editing::Integer | Editing::String) => (
            vec![ck_primary("Enter", "commit")],
            vec![ck("Esc", "cancel")],
        ),
        Some(Editing::AddProvider(_)) => (
            vec![
                ck("Tab", "field"),
                ck_primary("Enter", "choose/commit"),
                ck("Space", "toggle"),
            ],
            vec![ck("Esc", "cancel")],
        ),
        Some(Editing::ProviderDetail { .. }) => (
            vec![
                ck("↑↓", "navigate"),
                ck_primary("Space", "toggle"),
                ck("x", "delete"),
            ],
            vec![ck("Esc", "close")],
        ),
        None if is_providers_section(state.current_section()) => (
            vec![
                ck("↑↓", "navigate"),
                ck_primary("Enter", "details"),
                ck("n", "new"),
                ck("x", "remove"),
            ],
            vec![ck("^S", "save"), ck("Esc", "close")],
        ),
        None => (
            vec![
                ck("←→", "section"),
                ck("↑↓", "navigate"),
                ck_primary("Enter", "edit"),
                ck("Space", "toggle"),
                ck("d", "reset"),
                ck("/", "search"),
            ],
            vec![ck("^S", "save"), ck("Esc", "close")],
        ),
    }
}

fn render_config_keymap(main: &[ConfigKey], system: &[ConfigKey], width: usize) -> Line<'static> {
    const KEY_NORMAL: Color = Color::White;
    const KEY_PRIMARY: Color = Color::Cyan;
    const KEY_SYSTEM: Color = Color::Yellow;
    const LABEL: Color = Color::DarkGray;
    const SEP: &str = " · ";
    const CAT_SEP: &str = "  │  ";

    let mut right: Vec<Span<'static>> = Vec::new();
    let mut right_w: usize = 0;
    for (i, k) in system.iter().enumerate() {
        if i > 0 {
            right.push(Span::styled(SEP.to_string(), Style::default().fg(LABEL)));
            right_w += SEP.width();
        }
        // System keys use yellow to stand out
        let color = KEY_SYSTEM;
        right.push(Span::styled(
            k.glyph.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        right.push(Span::raw(" "));
        right.push(Span::styled(
            k.action.to_string(),
            Style::default().fg(LABEL),
        ));
        right_w += k.glyph.width() + 1 + k.action.width();
    }

    let cat_sep_w = CAT_SEP.width();
    let budget = width.saturating_sub(cat_sep_w + right_w);

    let mut left: Vec<Span<'static>> = Vec::new();
    let mut left_w: usize = 0;
    for (i, k) in main.iter().enumerate() {
        let sep_w = if i == 0 { 0 } else { SEP.width() };
        let entry_w = k.glyph.width() + 1 + k.action.width();
        if left_w + sep_w + entry_w > budget {
            break;
        }
        if i > 0 {
            left.push(Span::styled(SEP.to_string(), Style::default().fg(LABEL)));
        }
        // Primary keys use cyan (focus color), others use white
        let color = if k.primary { KEY_PRIMARY } else { KEY_NORMAL };
        let style = if k.primary {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };
        left.push(Span::styled(k.glyph.to_string(), style));
        left.push(Span::raw(" "));
        left.push(Span::styled(
            k.action.to_string(),
            Style::default().fg(LABEL),
        ));
        left_w += sep_w + entry_w;
    }

    let fill = width.saturating_sub(left_w + cat_sep_w + right_w);
    let mut spans = left;
    if fill > 0 {
        spans.push(Span::styled(
            format!("{}{}", CAT_SEP, " ".repeat(fill)),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        spans.push(Span::styled(
            CAT_SEP.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.extend(right);
    Line::from(spans)
}

fn footer_line(state: &ConfigPanelState, width: usize) -> Line<'static> {
    let (main, system) = footer_bindings(state);
    render_config_keymap(&main, &system, width)
}

fn header_line(path: &Path, width: u16) -> Line<'static> {
    let path_str = path.display().to_string();
    let title = "⚙ Settings";
    let right_w = (width as usize).saturating_sub(title.width() + 4);
    let right = middle_ellipsis(&path_str, right_w);
    top_rule_with_left_spans(
        vec![Span::styled(
            title.to_string(),
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD),
        )],
        Some(&right),
        width,
    )
}

fn visible_fields(state: &ConfigPanelState) -> Vec<usize> {
    field_indices_for(state.current_section())
}

fn field_indices_for(section: &str) -> Vec<usize> {
    FIELDS
        .iter()
        .enumerate()
        .filter_map(|(idx, meta)| (meta.section == section).then_some(idx))
        .collect()
}

fn dirty_count(state: &ConfigPanelState) -> usize {
    let field_count = FIELDS
        .iter()
        .filter(|meta| match meta.key {
            "meta.version" => false,
            _ => state.source_for(meta) == "override",
        })
        .count();
    if state.config.providers.is_explicit() {
        field_count + state.config.providers.value().len().max(1)
    } else {
        field_count
    }
}

fn ellipsize_end(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in value.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw + 1 > width {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

fn middle_ellipsis(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return ellipsize_end(value, width);
    }
    let chars: Vec<char> = value.chars().collect();
    let left = width.saturating_sub(1) / 2;
    let right = width.saturating_sub(1).saturating_sub(left);
    let prefix: String = chars.iter().take(left).collect();
    let suffix: String = chars
        .iter()
        .rev()
        .take(right)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

fn providers_body_lines(
    state: &ConfigPanelState,
    width: usize,
    body_h: usize,
) -> Vec<Line<'static>> {
    let lines = providers::get_lines(&state.config, &state.folded_vendors);
    if lines.is_empty() {
        state.providers_scroll.set(0);
        state.providers_body_h.set(body_h);
        return vec![Line::from(dim(
            "  no providers entries · operator-funded providers go here",
        ))];
    }
    state.providers_body_h.set(body_h);

    let total = lines.len();
    let cursor = state.providers_cursor.min(total.saturating_sub(1));
    let mut scroll = state.providers_scroll.get().min(total.saturating_sub(1));
    let inner_h = body_h.max(1);
    let needs_top_indicator = |scroll: usize| scroll > 0;
    let needs_bottom_indicator = |scroll: usize, capacity: usize| scroll + capacity < total;

    if cursor < scroll {
        scroll = cursor;
    }
    let mut content_h = inner_h;
    loop {
        let mut budget = content_h;
        if needs_top_indicator(scroll) {
            budget = budget.saturating_sub(1);
        }
        if needs_bottom_indicator(scroll, budget) {
            budget = budget.saturating_sub(1);
        }
        let last_visible = scroll + budget;
        if cursor >= last_visible {
            scroll = (cursor + 1).saturating_sub(budget);
        } else if scroll + budget > total && scroll > 0 {
            scroll = total.saturating_sub(budget);
        } else {
            break;
        }
        if content_h == 0 {
            break;
        }
        content_h = inner_h;
    }
    state.providers_scroll.set(scroll);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut budget = inner_h;
    if needs_top_indicator(scroll) {
        out.push(Line::from(dim(format!("  ↑ {scroll} more above"))));
        budget = budget.saturating_sub(1);
    }
    let bottom_reserved = if scroll + budget < total { 1 } else { 0 };
    let visible_count = budget.saturating_sub(bottom_reserved);
    let end = (scroll + visible_count).min(total);
    for (idx, line) in lines.iter().enumerate().take(end).skip(scroll) {
        out.push(providers::format_line(
            line,
            idx == state.providers_cursor,
            width,
        ));
    }
    if bottom_reserved == 1 {
        let remaining = total.saturating_sub(end);
        out.push(Line::from(dim(format!("  ↓ {remaining} more below"))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render_to_text(state: &ConfigPanelState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), state))
            .unwrap();
        terminal.backend().to_string()
    }

    fn state_with_overrides() -> ConfigPanelState {
        let mut config = Config::baked_defaults();
        mutate::set_value(
            &mut config,
            "ntfy.topic",
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        mutate::set_value(&mut config, "ntfy.detail_mode", "minimal").unwrap();
        mutate::set_value(
            &mut config,
            "paths.sessions_root",
            "$HOME/.codexize/sessions/with/a/very/long/path/that/wraps",
        )
        .unwrap();
        ConfigPanelState::open_at(&config, PathBuf::from("/tmp/example/config.toml"), None)
    }

    #[test]
    fn redesigned_panel_opens_on_general_settings_with_friendly_tabs() {
        let state = state_with_overrides();
        assert_eq!(state.current_section_name(), "general");

        let text = render_to_text(&state, 100, 18);
        assert!(text.contains("General"));
        assert!(text.contains("Models"));
        assert!(text.contains("Notifications"));
        assert!(text.contains("Agents"));
        assert!(text.contains("System"));
        assert!(
            text.contains("Full review cadence"),
            "general settings should use friendly labels: {text}"
        );
        assert!(
            !text.contains("full_review_interval"),
            "raw config labels should not be visible in the common page: {text}"
        );
    }

    #[test]
    fn notification_page_uses_friendly_names_not_raw_keys() {
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.select_first_field_in_current_section();

        let text = render_to_text(&state, 100, 18);
        assert!(text.contains("Retry attempts"));
        assert!(text.contains("Topic"));
        assert!(
            !text.contains("retry_attempts"),
            "raw snake_case labels should not render: {text}"
        );
        assert!(
            !text.contains("detail_mode"),
            "raw snake_case labels should not render: {text}"
        );
    }

    #[test]
    fn model_tab_enter_opens_detail_drawer_with_enabled_focused() {
        // Models page Enter (and Space) opens the per-provider detail drawer
        // rather than directly toggling. The drawer's cursor lands on the
        // first user-toggleable property — Enabled, index 0 — so the most
        // common operation (Space, Space) still flips availability.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        state.providers_cursor = providers::get_lines(&state.config, &state.folded_vendors)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .expect("provider row");

        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            state.editing,
            Some(Editing::ProviderDetail { cursor: 0 })
        ));

        // Space inside the drawer flips the focused property and stays open.
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let lines = providers::get_lines(&state.config, &state.folded_vendors);
        let providers::ProvidersLine::Provider { entry, .. } = &lines[state.providers_cursor]
        else {
            panic!("cursor should still point at provider");
        };
        assert!(!entry.enabled, "space in drawer should flip availability");
        assert!(state.dirty);
        assert!(matches!(
            state.editing,
            Some(Editing::ProviderDetail { .. })
        ));
    }

    #[test]
    fn model_tab_x_deletes_user_added_provider() {
        let mut config = Config::baked_defaults();
        config.providers = Override::explicit(vec![crate::data::config::schema::ProviderEntry {
            cli: crate::selection::CliKind::Opencode,
            launch_name: "custom-opus".to_string(),
            model: "claude-opus-4.7".to_string(),
            subscription: crate::selection::SubscriptionKind::Claude,
            enabled: true,
            free: false,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_mapping: Default::default(),
            quota_lookup_key: None,
            display_order: 0,
        }]);
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        state.providers_cursor = providers::get_lines(&state.config, &state.folded_vendors)
            .iter()
            .position(|l| {
                matches!(
                    l,
                    providers::ProvidersLine::Provider { entry, is_baked: false, .. }
                        if entry.launch_name == "custom-opus"
                )
            })
            .expect("custom provider row");

        state.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert!(
            !state
                .config
                .providers
                .value()
                .iter()
                .any(|entry| entry.launch_name == "custom-opus"),
            "custom provider should be removed from overrides"
        );
        assert!(state.dirty);
    }

    #[test]
    fn ctrl_i_and_tab_both_switch_pages() {
        let mut state = state_with_overrides();
        assert_eq!(state.current_section(), "general");

        state.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL));
        assert_eq!(state.current_section(), "models");

        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(state.current_section(), "notifications");
    }

    #[test]
    fn tab_cycles_sections_with_wrap() {
        let mut state = state_with_overrides();
        state.selected_section = 0;
        state.select_first_field_in_current_section();
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(state.current_section(), SECTIONS[1]);
        for _ in 0..SECTIONS.len() - 1 {
            state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        assert_eq!(state.current_section(), SECTIONS[0]);
    }

    #[test]
    fn shift_tab_cycles_sections_reverse() {
        let mut state = state_with_overrides();
        state.selected_section = 0;
        state.select_first_field_in_current_section();
        state.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(state.current_section(), *SECTIONS.last().unwrap());
    }

    #[test]
    fn bracket_keys_jump_first_last_section() {
        let mut state = state_with_overrides();
        state.selected_section = 5;
        state.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(state.selected_section, 0);
        state.handle_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(state.selected_section, SECTIONS.len() - 1);
    }

    #[test]
    #[allow(non_snake_case)]
    fn g_G_jumps_first_last_field() {
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.selected_field = 5;
        state.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        let fields = field_indices_for(state.current_section());
        assert_eq!(state.selected_field, fields[0]);
        state.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(state.selected_field, *fields.last().unwrap());
    }

    #[test]
    fn narrow_width_refuses_to_render_panel() {
        let state = state_with_overrides();
        let text = render_to_text(&state, 49, 6);
        assert!(text.contains(terminal_too_narrow_message()));
    }

    fn focus_field(state: &mut ConfigPanelState, key: &str) {
        let field_idx = FIELDS.iter().position(|f| f.key == key).unwrap();
        state.selected_section = SECTIONS
            .iter()
            .position(|s| *s == FIELDS[field_idx].section)
            .unwrap();
        state.selected_field = field_idx;
    }

    #[test]
    fn arrow_keys_navigate_between_section_tabs() {
        // ←/→ and h/l mirror Tab/Shift-Tab for section navigation. They
        // never enter edit mode and never mutate the focused field's value.
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let section_before = state.selected_section;

        state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            state.selected_section,
            (section_before + 1) % SECTIONS.len()
        );
        assert!(state.editing.is_none());

        state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(state.selected_section, section_before);

        state.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(
            state.selected_section,
            (section_before + 1) % SECTIONS.len()
        );

        state.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(state.selected_section, section_before);
    }

    #[test]
    fn horizontal_arrows_dont_cycle_enum_values() {
        // Horizontal movement switches sections, so the original value lives
        // on whichever section the cursor leaves behind.
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        let value_before = state.value_for(state.current_meta().unwrap());

        // After ←, the section changes and current_meta() points at a
        // different field. We re-focus the original field and confirm
        // its persisted value is unchanged.
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('h'),
            KeyCode::Char('l'),
        ] {
            state.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            focus_field(&mut state, "ntfy.detail_mode");
            assert_eq!(
                state.value_for(state.current_meta().unwrap()),
                value_before,
                "{code:?} mutated detail_mode"
            );
        }
    }

    #[test]
    fn enter_on_bool_flips_inline_without_dropdown() {
        // Bools toggle directly: a two-row dropdown is dead weight when the
        // only options are true/false, so Enter (and Space) flips the value
        // and stays in nav mode.
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let value_before = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none(), "bool flip must not enter editing");
        let after = state.value_for(state.current_meta().unwrap());
        assert_ne!(after, value_before, "bool value should have flipped");
        assert!(state.dirty);
    }

    #[test]
    fn space_on_bool_flips_inline() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let before = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(state.editing.is_none());
        assert_ne!(state.value_for(state.current_meta().unwrap()), before);
    }

    #[test]
    fn enter_on_enum_opens_dropdown_with_variants() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Editing::Choice { options, .. }) = state.editing.as_ref() else {
            panic!("expected Choice editing state");
        };
        assert!(options.iter().any(|o| o == "detailed"));
        assert!(options.iter().any(|o| o == "minimal"));
    }

    #[test]
    fn enter_on_integer_enters_inline_edit() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        let value = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(state.editing, Some(Editing::Integer)));
        assert_eq!(state.edit_buffer, value);
    }

    #[test]
    fn enter_on_string_enters_inline_edit() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.server");
        let value = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(state.editing, Some(Editing::String)));
        assert_eq!(state.edit_buffer, value);
    }

    #[test]
    fn inline_edit_enter_commits_buffer() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.edit_buffer = "7".to_string();
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none());
        assert_eq!(state.value_for(state.current_meta().unwrap()), "7");
    }

    #[test]
    fn inline_edit_invalid_integer_blocks_save() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.edit_buffer = "0".to_string();
        assert!(
            state
                .current_validation_error()
                .unwrap()
                .contains("cannot save")
        );
    }

    #[test]
    fn d_resets_overridden_enum_to_default() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        assert_eq!(state.source_for(state.current_meta().unwrap()), "override");
        state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(state.source_for(state.current_meta().unwrap()), "(def)");
    }

    #[test]
    fn rendered_overrides_show_dirty_marker_source_tag_and_tab_suffix() {
        // The override fixture sets ntfy.topic + ntfy.detail_mode + paths.sessions_root.
        // Verify the three operator-facing override signals: (1) `●` dot in
        // the dirty column, (2) `(overridden)` chip on the value, (3) `●`
        // suffix on tab-bar entries for override-bearing sections.
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.select_first_field_in_current_section();
        let text = render_to_text(&state, 120, 20);
        assert!(
            text.contains(" ● Topic"),
            "missing override dot on topic row: {text}"
        );
        assert!(
            text.contains(" ● Detail"),
            "missing override dot on detail_mode row: {text}"
        );
        assert!(
            text.contains("(overridden)"),
            "missing override chip on value: {text}"
        );
        // Active tab shows brackets: [Notifications], inactive tabs show plain text
        assert!(
            text.contains("[Notifications] ●") || text.contains("Notifications ●"),
            "missing override marker on notifications tab: {text}"
        );
        assert!(
            text.contains("System ●"),
            "missing override marker on system tab: {text}"
        );
    }

    #[test]
    fn slash_opens_search_and_typing_filters_results() {
        let mut state = state_with_overrides();
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        let search = state.searching.as_ref().expect("search opened");
        // Empty query lists every field.
        assert_eq!(search.results.len(), FIELDS.len());

        for c in "retry".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let search = state.searching.as_ref().expect("still searching");
        assert!(!search.results.is_empty());
        for &idx in &search.results {
            let key = FIELDS[idx].key;
            assert!(
                key.contains("retry") || FIELDS[idx].label.contains("retry"),
                "result {key} must match query 'retry'"
            );
        }
    }

    #[test]
    fn enter_in_search_jumps_to_field_across_sections() {
        let mut state = state_with_overrides();
        // Start on notifications; jump to a field in system via search.
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.select_first_field_in_current_section();
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "max_topics".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.searching.is_none(), "Enter must close search");
        assert_eq!(state.current_section_name(), "system");
        assert_eq!(
            state.current_meta().unwrap().key,
            "memory.max_topics_per_read"
        );
    }

    #[test]
    fn esc_in_search_closes_overlay_without_jumping() {
        let mut state = state_with_overrides();
        let section_before = state.selected_section;
        let field_before = state.selected_field;
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "memory".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.searching.is_none());
        assert_eq!(state.selected_section, section_before);
        assert_eq!(state.selected_field, field_before);
    }

    #[test]
    fn providers_section_renders_entries_with_unmatched_warning() {
        // Two entries: one matched against the known universe, one that
        // points at a row no provider serves yet. The matched entry renders
        // bare; the unmatched entry trails the soft-warning suffix.
        let mut config = Config::baked_defaults();
        config.providers = Override::explicit(vec![crate::data::config::schema::ProviderEntry {
            cli: crate::selection::CliKind::Claude,
            launch_name: "claude-opus-4.7".to_string(),
            model: "claude-opus-4.7".to_string(),
            subscription: crate::selection::SubscriptionKind::Claude,
            enabled: true,
            free: false,
            official: true,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_mapping: Default::default(),
            quota_lookup_key: None,
            display_order: 0,
        }]);
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();

        let text = render_to_text(&state, 120, 18);
        // Vendor and model headers render on separate lines (no longer
        // "claude · claude-opus-4.7" together).
        assert!(
            text.contains("▾ claude"),
            "missing claude vendor header: {text}"
        );
        assert!(
            text.contains("▾ claude-opus-4.7"),
            "missing model header under vendor: {text}"
        );
        assert!(
            text.contains("✓ claude"),
            "missing enabled provider row: {text}"
        );
        assert!(
            text.contains("claude-opus-4.7"),
            "missing launch_name on row: {text}"
        );
        assert!(
            // The chip strip shows only the ticked eligibility flags;
            // built-in, official, paid and unofficial are suppressed.
            text.contains("tough") && text.contains("effort"),
            "missing provider eligibility chips: {text}"
        );
    }

    #[test]
    fn providers_section_shows_baked_models_by_default() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();

        // Height tall enough to render every baked row + the Add Provider
        // footer; the section now has 30 baked rows and grows over time.
        let text = render_to_text(&state, 120, 80);
        assert!(
            text.contains("▾ claude"),
            "should show claude vendor header: {text}"
        );
        assert!(
            text.contains("▾ claude-opus-4.7"),
            "should show baked model under vendor: {text}"
        );
        assert!(
            text.contains("+ New model"),
            "should show new-model button: {text}"
        );
    }

    #[test]
    fn providers_section_render_does_not_panic_at_multibyte_boundaries() {
        let config = Config::baked_defaults();
        let state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );

        for width in MIN_WIDTH..=140 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render_to_text(&state, width, 24)
            }));
            assert!(
                result.is_ok(),
                "models page render panicked at width {width}"
            );
        }
    }

    #[test]
    fn add_provider_modal_populates_models_from_baked_universe() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();

        let lines = providers::get_lines(&state.config, &state.folded_vendors);
        let add_idx = lines
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::AddAction))
            .expect("AddAction line present");
        state.providers_cursor = add_idx;
        state.activate_provider_line();

        let Some(Editing::AddProvider(editor)) = state.editing.as_ref() else {
            panic!("AddProvider modal should be active");
        };
        assert!(
            !editor.available_models.is_empty(),
            "modal should derive models from the baked + override universe"
        );
        assert!(
            editor
                .available_models
                .iter()
                .any(|(v, m)| v == "claude" && m == "claude-opus-4.7"),
            "expected baked claude/claude-opus-4.7 in available models: {:?}",
            editor.available_models,
        );
        assert_eq!(editor.subscription, editor.available_models[0].0);
        assert_eq!(editor.model, editor.available_models[0].1);
    }

    #[test]
    fn providers_group_by_model_vendor_not_subscription_label() {
        // opencode-go is a subscription (it bills through the opencode pool),
        // not a vendor — the actual model vendor is deepseek/minimax/etc.
        // Now that providers render as vendor → model → entry, the test
        // checks two facts: a `deepseek` vendor header exists and its
        // child model header is `deepseek-v4-flash`; and `opencode-go`
        // never appears as a vendor header.
        let config = Config::baked_defaults();
        // Pass an empty fold-set so every model header is emitted and we
        // can verify the vendor → model mapping.
        let folded_v = std::collections::BTreeSet::new();
        let lines = providers::get_lines(&config, &folded_v);
        let mut seen_vendors: Vec<String> = Vec::new();
        let mut seen_pairs: Vec<(String, String)> = Vec::new();
        let mut current_vendor: Option<String> = None;
        for line in &lines {
            match line {
                providers::ProvidersLine::VendorHeader { vendor, .. } => {
                    seen_vendors.push(vendor.clone());
                    current_vendor = Some(vendor.clone());
                }
                providers::ProvidersLine::ModelHeader { model, .. } => {
                    if let Some(v) = &current_vendor {
                        seen_pairs.push((v.clone(), model.clone()));
                    }
                }
                _ => {}
            }
        }
        assert!(
            seen_pairs
                .iter()
                .any(|(v, m)| v == "deepseek" && m == "deepseek-v4-flash"),
            "expected deepseek-v4-flash filed under vendor 'deepseek', got: {seen_pairs:?}"
        );
        assert!(
            !seen_vendors.iter().any(|v| v == "opencode-go"),
            "subscription label 'opencode-go' must not appear as a vendor: {seen_vendors:?}"
        );
    }

    #[test]
    fn provider_row_renders_subscription_cli_and_launch_name_separately() {
        // Each entry under a (vendor, model) group must display all three
        // facets — subscription, CLI, launch_name — so the user can tell
        // a kimi-via-Kimi pool entry apart from a (hypothetical)
        // kimi-via-Opencode entry at a glance.
        let mut config = Config::baked_defaults();
        // Inject an opencode-routed kimi-k2.6 row alongside the baked
        // kimi-via-Kimi row so the group has two distinct entries.
        let mut overrides = config.providers.value().clone();
        overrides.push(crate::data::config::schema::ProviderEntry {
            cli: crate::selection::CliKind::Opencode,
            launch_name: "opencode-go/kimi-k2.6".to_string(),
            model: "kimi-k2.6".to_string(),
            subscription: crate::selection::SubscriptionKind::OpencodeGo,
            enabled: true,
            free: false,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: Default::default(),
            quota_lookup_key: None,
            display_order: 0,
        });
        config.providers = Override::explicit(overrides);

        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();

        let text = render_to_text(&state, 120, 80);
        // With the new tree layout, vendor and model render on separate
        // lines: "▾ kimi" / "  ▾ kimi-k2.6".
        assert!(
            text.contains("▾ kimi"),
            "expected kimi vendor header: {text}"
        );
        assert!(
            text.contains("▾ kimi-k2.6"),
            "expected kimi-k2.6 model header under vendor: {text}"
        );
        // Provider rows render subscription · cli · launch_name as
        // padded columns. Substring-match on each token (the column
        // widths put two or more spaces between them).
        assert!(
            text.contains("moonshotai") && text.contains("kimi-latest"),
            "expected built-in kimi entry: {text}"
        );
        assert!(
            text.contains("opencode-go") && text.contains("opencode-go/kimi-k2.6"),
            "expected opencode-routed kimi entry under the same group: {text}"
        );
    }

    #[test]
    fn providers_pagination_keeps_focused_row_in_viewport() {
        // 30+ baked rows + group headers can't fit a typical viewport. As
        // the cursor moves down, the top of the window must follow so the
        // focused row stays visible and the bottom indicator updates.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();

        // Render once to seed body_h.
        let _ = render_to_text(&state, 100, 18);
        let body_h = state.providers_body_h.get();
        assert!(body_h > 4, "viewport should hold a handful of rows");

        // Push the cursor far past the visible window.
        for _ in 0..(body_h * 3) {
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let lines_total = providers::get_lines(&state.config, &state.folded_vendors).len();
        let cursor = state.providers_cursor;
        assert!(cursor < lines_total);

        // Render again so providers_body_lines snaps the scroll offset.
        let _ = render_to_text(&state, 100, 18);
        let scroll = state.providers_scroll.get();
        assert!(
            cursor >= scroll && cursor < scroll + body_h,
            "cursor {cursor} must be inside [{scroll}, {}]",
            scroll + body_h
        );
    }

    #[test]
    fn providers_pagination_indicator_visible_when_overflowing() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        let text = render_to_text(&state, 100, 14);
        assert!(
            text.contains("more below"),
            "expected bottom scroll indicator: {text}"
        );
    }

    #[test]
    fn providers_navigation_skips_model_headers_lands_on_vendor_headers() {
        // VendorHeader is interactive (Space toggles fold) — cursor lands.
        // ModelHeader is purely structural — cursor walks past it.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        let lines = providers::get_lines(&state.config, &state.folded_vendors);
        state.providers_cursor = lines
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .unwrap();
        let mut hit_vendor = false;
        for _ in 0..40 {
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            assert!(
                !matches!(
                    lines.get(state.providers_cursor),
                    Some(providers::ProvidersLine::ModelHeader { .. })
                ),
                "cursor {} landed on a model header",
                state.providers_cursor
            );
            if matches!(
                lines.get(state.providers_cursor),
                Some(providers::ProvidersLine::VendorHeader { .. })
            ) {
                hit_vendor = true;
            }
        }
        assert!(hit_vendor, "j should walk through vendor headers");
    }

    #[test]
    fn provider_detail_drawer_skips_baked_locked_rows_during_navigation() {
        // Built-in providers expose `Official` / `Free` as derived flags
        // rather than user-controllable toggles; the cursor must skip those
        // rows so j/k always lands on something Space can flip.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        state.providers_cursor = providers::get_lines(&state.config, &state.folded_vendors)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { is_baked: true, .. }))
            .expect("baked provider row");

        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let initial_cursor = match state.editing {
            Some(Editing::ProviderDetail { cursor }) => cursor,
            _ => panic!("expected detail drawer"),
        };
        assert!(!PROVIDER_TOGGLES[initial_cursor].baked_locked);

        // Walk one full circuit; every visited cursor must be unlocked.
        for _ in 0..PROVIDER_TOGGLES.len() {
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            let cursor = match state.editing {
                Some(Editing::ProviderDetail { cursor }) => cursor,
                _ => panic!("drawer should stay open"),
            };
            assert!(
                !PROVIDER_TOGGLES[cursor].baked_locked,
                "j on baked drawer landed on locked toggle {} (idx {cursor})",
                PROVIDER_TOGGLES[cursor].label
            );
        }
    }

    #[test]
    fn add_provider_modal_can_compose_opencode_for_kimi_via_independent_pickers() {
        // The user complaint: kimi has no opencode entry. With the modal's
        // form-style focus walk, the user can pick model=kimi-k2.6 from the
        // baked universe, then Tab to subscription and cycle to OpencodeGo,
        // then Tab to CLI and cycle to Opencode, then type the launch name.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        let lines = providers::get_lines(&state.config, &state.folded_vendors);
        let add_idx = lines
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::AddAction))
            .expect("add row");
        state.providers_cursor = add_idx;
        state.activate_provider_line();

        // Open the Model dropdown (Enter on the focused Model field) and
        // navigate to kimi-k2.6, then Enter to commit the selection.
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for _ in 0..50 {
            if let Some(Editing::AddProvider(editor)) = state.editing.as_ref()
                && editor.open_dropdown == Some(providers::AddProviderField::Model)
                && editor
                    .dropdown_options(providers::AddProviderField::Model)
                    .get(editor.dropdown_cursor)
                    == Some(&"kimi-k2.6".to_string())
            {
                break;
            }
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(state.editing.as_ref(),
                Some(Editing::AddProvider(e)) if e.model == "kimi-k2.6"),
            "model dropdown did not commit kimi-k2.6"
        );

        // Tab to Subscription, open its dropdown, walk to opencode-go, commit.
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for _ in 0..SUBSCRIPTION_OPTIONS_COUNT {
            if let Some(Editing::AddProvider(editor)) = state.editing.as_ref()
                && editor
                    .dropdown_options(providers::AddProviderField::Subscription)
                    .get(editor.dropdown_cursor)
                    == Some(&"opencode-go".to_string())
            {
                break;
            }
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(state.editing.as_ref(),
                Some(Editing::AddProvider(e)) if e.subscription == "opencode-go"),
            "subscription dropdown did not commit opencode-go"
        );

        // Tab to CLI, open dropdown, walk to opencode, commit.
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for _ in 0..CLI_OPTIONS_COUNT {
            if let Some(Editing::AddProvider(editor)) = state.editing.as_ref()
                && editor
                    .dropdown_options(providers::AddProviderField::Cli)
                    .get(editor.dropdown_cursor)
                    == Some(&"opencode".to_string())
            {
                break;
            }
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Tab to Launch name (skipping Official/Free) and type a value, then Enter to commit the form.
        for _ in 0..3 {
            state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        for c in "opencode-go/kimi-k2.6".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none(), "modal should close after commit");

        let added = state.config.providers.value().iter().find(|e| {
            e.model == "kimi-k2.6"
                && matches!(e.cli, crate::selection::CliKind::Opencode)
                && matches!(
                    e.subscription,
                    crate::selection::SubscriptionKind::OpencodeGo
                )
        });
        assert!(
            added.is_some(),
            "opencode-routed kimi entry should be persisted"
        );
    }

    const SUBSCRIPTION_OPTIONS_COUNT: usize = 6; // SubscriptionKind variants
    const CLI_OPTIONS_COUNT: usize = 5;

    #[test]
    fn q_aliases_esc_in_nav_and_picker_modes_but_not_in_text_input() {
        // Nav mode: q closes the panel, just like Esc.
        let config = Config::baked_defaults();
        let mut state =
            ConfigPanelState::open_at(&config, PathBuf::from("/tmp/example/config.toml"), None);
        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(outcome, PanelOutcome::Close));

        // Choice picker: q cancels the dropdown without committing.
        let mut state =
            ConfigPanelState::open_at(&config, PathBuf::from("/tmp/example/config.toml"), None);
        focus_field(&mut state, "ntfy.detail_mode");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(state.editing, Some(Editing::Choice { .. })));
        state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(
            state.editing.is_none(),
            "q should close the choice dropdown"
        );

        // Provider detail drawer: q closes.
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.expand_all_vendors_for_test();
        state.providers_cursor = providers::get_lines(&state.config, &state.folded_vendors)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .unwrap();
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            state.editing,
            Some(Editing::ProviderDetail { .. })
        ));
        state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(state.editing.is_none(), "q should close the detail drawer");

        // Inline text edit: q is a literal character in the buffer, not Esc.
        let mut state =
            ConfigPanelState::open_at(&config, PathBuf::from("/tmp/example/config.toml"), None);
        focus_field(&mut state, "ntfy.server");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(state.editing, Some(Editing::String)));
        let before_len = state.edit_buffer.len();
        state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(
            matches!(state.editing, Some(Editing::String)),
            "q in string edit must be a literal — editing must remain open"
        );
        assert_eq!(
            state.edit_buffer.len(),
            before_len + 1,
            "q should append to the edit buffer in text-input mode"
        );
    }

    #[test]
    fn shift_r_regenerates_topic_now_that_q_is_an_esc_alias() {
        // ntfy.topic regenerate moved off `q` (now an Esc alias) onto `R`.
        let config = Config::baked_defaults();
        let mut state =
            ConfigPanelState::open_at(&config, PathBuf::from("/tmp/example/config.toml"), None);
        focus_field(&mut state, "ntfy.topic");
        state.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
        assert!(
            matches!(state.conflict, Some(ConflictBanner::RegenerateTopicPrompt)),
            "Shift-R on ntfy.topic should open the regenerate prompt"
        );
        // q on the prompt cancels it (Esc alias).
        state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(state.conflict.is_none(), "q should dismiss the prompt");
    }

    #[test]
    fn providers_tree_renders_vendor_then_models_under_it() {
        // The vendor section header appears once and the models beneath
        // it drop the "<vendor> ·" prefix so the tree reads cleanly.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        let text = render_to_text(&state, 120, 50);

        // Vendor header is bare; model header is indented and lacks the
        // vendor prefix.
        assert!(text.contains("▾ claude"));
        assert!(text.contains("  ▾ claude-opus-4.1"));
        assert!(
            !text.contains("▾ claude · claude-opus-4.1"),
            "model header should not repeat the vendor name"
        );
    }

    #[test]
    fn provider_rows_align_subscription_column_with_distinct_color() {
        // moonshotai (10 chars) and claude (6 chars) both render padded
        // to 11 chars so the cli column starts at the same screen offset.
        // The rendered text strips style, so we just check the prefix
        // padding here; the color is asserted via the snapshot.
        let mut config = Config::baked_defaults();
        let mut overrides = config.providers.value().clone();
        overrides.push(crate::data::config::schema::ProviderEntry {
            cli: crate::selection::CliKind::Opencode,
            launch_name: "opencode-go/kimi-k2.6".to_string(),
            model: "kimi-k2.6".to_string(),
            subscription: crate::selection::SubscriptionKind::OpencodeGo,
            enabled: true,
            free: false,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: Default::default(),
            quota_lookup_key: None,
            display_order: 0,
        });
        config.providers = Override::explicit(overrides);

        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        let text = render_to_text(&state, 140, 80);

        // After the focus marker + ✓, the subscription label sits in a
        // padded column. moonshotai is 10 chars wide so it carries one
        // trailing space inside the 11-col cell; opencode-go is 11 chars
        // wide so it fills the cell exactly. Both rows then drop two
        // gap spaces before the cli column.
        assert!(
            text.contains("✓ moonshotai   kimi"),
            "expected moonshotai padded to subscription column: {text}"
        );
        assert!(
            text.contains("✓ opencode-go  opencode"),
            "expected opencode-go padded to subscription column: {text}"
        );
    }

    #[test]
    fn add_provider_modal_n_key_opens_modal() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        state.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(
            matches!(state.editing, Some(Editing::AddProvider(_))),
            "n should open the Add Provider modal"
        );
    }

    #[test]
    fn add_provider_modal_enter_on_enum_field_opens_dropdown() {
        // Enter on Model, Subscription, or CLI opens that field's
        // dropdown popup; Enter on LaunchName commits the modal.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        state.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        // Default focus is Model — Enter opens the Model dropdown.
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            state.editing.as_ref(),
            Some(Editing::AddProvider(e))
                if e.open_dropdown == Some(providers::AddProviderField::Model)
        ));

        // Esc closes the dropdown but keeps the modal open.
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(
            state.editing.as_ref(),
            Some(Editing::AddProvider(e)) if e.open_dropdown.is_none()
        ));

        // Tab to Subscription field; Enter opens the Subscription dropdown.
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            state.editing.as_ref(),
            Some(Editing::AddProvider(e))
                if e.open_dropdown == Some(providers::AddProviderField::Subscription)
        ));
    }

    #[test]
    fn direct_edit_no_e_prefix_required() {
        // Panel opens directly editable: Enter on the first numeric
        // field enters inline-edit without any prior `e` keystroke.
        let config = Config::baked_defaults();
        let mut state =
            ConfigPanelState::open_at(&config, PathBuf::from("/tmp/example/config.toml"), None);
        // Move to a known integer field.
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(state.editing, Some(Editing::Integer)),
            "Enter should start editing without an e-toggle"
        );
    }

    #[test]
    fn config_version_field_is_not_listed() {
        // The schema-version stamp is binary-managed and never useful in
        // the UI — it must not appear in the editable field list.
        assert!(
            !FIELDS.iter().any(|f| f.key == "meta.version"),
            "meta.version should be hidden from the field list"
        );
    }

    #[test]
    fn dirty_esc_pops_exit_modal_with_save_discard_cancel() {
        let mut state = state_with_overrides();
        // state_with_overrides isn't dirty (overrides are baked in), so
        // stage a real change to flip the dirty bit.
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.set_edit_buffer_for_test("9".to_string());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.dirty);

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(outcome, PanelOutcome::KeepOpen));
        assert!(matches!(
            state.conflict,
            Some(ConflictBanner::ExitPrompt { .. })
        ));
    }

    #[test]
    fn exit_modal_d_key_discards_and_closes() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.set_edit_buffer_for_test("11".to_string());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(outcome, PanelOutcome::Close));
        assert!(state.conflict.is_none());
    }

    #[test]
    fn exit_modal_c_key_cancels_and_keeps_panel_open() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.set_edit_buffer_for_test("11".to_string());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(matches!(outcome, PanelOutcome::KeepOpen));
        assert!(state.conflict.is_none(), "modal should close on cancel");
        assert!(state.dirty, "dirty flag must stay set after cancel");
    }

    #[test]
    fn providers_open_with_every_vendor_section_folded_by_default() {
        let config = Config::baked_defaults();
        let state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        // Every known vendor name should be in the folded set so the
        // page lands closed-by-default, no exceptions.
        let expected = providers::all_vendors(&state.config);
        assert!(!expected.is_empty(), "baked defaults must seed vendors");
        for v in &expected {
            assert!(
                state.folded_vendors.contains(v),
                "vendor {v:?} should start folded"
            );
        }
    }

    #[test]
    fn space_on_vendor_header_toggles_fold_state() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.providers_cursor = 0; // first vendor header

        let lines = providers::get_lines(&state.config, &state.folded_vendors);
        let (target, folded): (String, bool) = match &lines[0] {
            providers::ProvidersLine::VendorHeader { vendor, folded } => (vendor.clone(), *folded),
            _ => panic!("expected first line to be a vendor header"),
        };
        assert!(folded, "vendor must start folded");

        // Space should expand it.
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            !state.folded_vendors.contains(&target),
            "space should expand the focused vendor"
        );

        // Space again folds it back.
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            state.folded_vendors.contains(&target),
            "space should fold the focused vendor again"
        );
    }

    #[test]
    fn provider_row_chip_strip_shows_only_ticked_flags() {
        // built-in and official are implicit defaults — never rendered.
        // Eligibility flags only show when ticked (no `paid`,
        // `unofficial`, etc.).
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.expand_all_vendors_for_test();
        let text = render_to_text(&state, 130, 30);

        for hidden in ["built-in", "official", "paid", "unofficial"] {
            assert!(
                !text.contains(hidden),
                "`{hidden}` should be suppressed from chip strip: {text}"
            );
        }
        // Eligibility flags do show when set — claude-opus rows are
        // tough+effort eligible.
        assert!(text.contains("tough"), "tough chip missing: {text}");
        assert!(text.contains("effort"), "effort chip missing: {text}");
    }

    #[test]
    fn save_detects_mtime_conflict_and_overwrite_writes_sparse_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut state = state_with_overrides();
        state.path = path.clone();
        crate::data::config::save_atomic_to(&path, &state.config).unwrap();
        state.opened_mtime = Some(SystemTime::UNIX_EPOCH);
        fs::write(&path, "[meta]\nversion = 1\n[ntfy]\nretry_attempts = 2\n").unwrap();

        state.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(
            state.conflict,
            Some(ConflictBanner::MtimeAdvanced)
        ));
        state.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        let saved = fs::read_to_string(path).unwrap();
        assert!(saved.contains("topic = "));
        assert!(saved.contains("detail_mode = \"minimal\""));
        assert!(!saved.contains("enabled = true"));
    }
}
