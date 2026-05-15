use crate::ui::palette::{self, PaletteCommand};
use crate::ui::widgets::picker::state::SessionEntry;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
pub(crate) fn visible_entries(entries: &[SessionEntry], show_archived: bool) -> Vec<&SessionEntry> {
    entries
        .iter()
        .filter(|e| show_archived || !e.archived)
        .collect()
}
pub(crate) fn selected_entry(
    entries: &[SessionEntry],
    show_archived: bool,
    selected: usize,
) -> Option<&SessionEntry> {
    visible_entries(entries, show_archived)
        .get(selected)
        .copied()
}
pub(crate) fn page_step(body_inner_height: usize) -> usize {
    // Before the first draw there is no measured body height yet. The
    // interactive run loop always draws before reading input, so callers
    // without a render pass conservatively get a zero-step page move.
    body_inner_height.saturating_sub(1)
}
pub(crate) fn palette_inner_rows(buffer: &str, selected_archived: bool) -> u16 {
    const MAX_OVERLAY_INNER: u16 = 12;
    let commands = palette_commands(selected_archived);
    let filtered = palette::filter(buffer, &commands);
    let suggestions = filtered.len().min(10) as u16;
    (1 + suggestions + 1).min(MAX_OVERLAY_INNER)
}
pub(crate) fn palette_overlay_height(
    buffer: &str,
    selected_archived: bool,
    total_height: u16,
) -> u16 {
    const LIST_RESERVE: u16 = 4;
    let inner = palette_inner_rows(buffer, selected_archived);
    let desired = inner + 2;
    let cap = total_height.saturating_sub(LIST_RESERVE).max(3);
    desired.min(cap).max(3)
}
pub(crate) fn palette_lines(
    buffer: &str,
    selected_archived: bool,
    width: u16,
    inner_h: u16,
) -> Vec<Line<'static>> {
    let commands = palette_commands(selected_archived);
    let ghost = palette::ghost_completion(buffer, &commands).unwrap_or("");
    let suffix = ghost.strip_prefix(buffer.trim()).unwrap_or("");
    let mut input = vec![
        Span::styled(
            ":",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(buffer.to_string()),
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
    let filtered = palette::filter(buffer, &commands);
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
pub(crate) fn palette_commands(selected_archived: bool) -> Vec<PaletteCommand> {
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
        PaletteCommand {
            name: "config",
            aliases: &["c"],
            help: "View or edit configuration",
            key_hint: None,
        },
    ];
    if selected_archived {
        commands.push(PaletteCommand {
            name: "restore",
            aliases: &["r"],
            help: "Restore selected archived session",
            key_hint: None,
        });
    }
    commands
}
#[cfg(test)]
#[path = "helpers_tests.rs"]
mod tests;
