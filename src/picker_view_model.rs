use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::app::palette::{self, PaletteCommand};
use crate::picker::SessionEntry;

pub fn visible_entries(entries: &[SessionEntry], show_archived: bool) -> Vec<&SessionEntry> {
    entries
        .iter()
        .filter(|e| show_archived || !e.archived)
        .collect()
}

pub fn selected_entry(
    entries: &[SessionEntry],
    show_archived: bool,
    selected: usize,
) -> Option<&SessionEntry> {
    visible_entries(entries, show_archived)
        .get(selected)
        .copied()
}

pub fn page_step(body_inner_height: usize) -> usize {
    // Before the first draw there is no measured body height yet. The
    // interactive run loop always draws before reading input, so callers
    // without a render pass conservatively get a zero-step page move.
    body_inner_height.saturating_sub(1)
}

pub fn palette_inner_rows(buffer: &str, selected_archived: bool) -> u16 {
    const MAX_OVERLAY_INNER: u16 = 12;
    let commands = palette_commands(selected_archived);
    let filtered = palette::filter(buffer, &commands);
    let suggestions = filtered.len().min(10) as u16;
    (1 + suggestions + 1).min(MAX_OVERLAY_INNER)
}

pub fn palette_overlay_height(buffer: &str, selected_archived: bool, total_height: u16) -> u16 {
    const LIST_RESERVE: u16 = 4;
    let inner = palette_inner_rows(buffer, selected_archived);
    let desired = inner + 2;
    let cap = total_height.saturating_sub(LIST_RESERVE).max(3);
    desired.min(cap).max(3)
}

pub fn palette_lines(
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

pub fn palette_commands(selected_archived: bool) -> Vec<PaletteCommand> {
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
mod tests {
    use super::*;
    use crate::state::{Modes, Phase};
    use std::time::SystemTime;

    fn entry(id: &str, archived: bool) -> SessionEntry {
        SessionEntry {
            session_id: id.to_string(),
            idea_summary: id.to_string(),
            current_phase: Phase::IdeaInput,
            modes: Modes::default(),
            last_modified: SystemTime::UNIX_EPOCH,
            archived,
        }
    }

    #[test]
    fn visible_entries_hides_archived_until_enabled() {
        let entries = vec![entry("active", false), entry("archived", true)];

        assert_eq!(visible_entries(&entries, false).len(), 1);
        assert_eq!(visible_entries(&entries, true).len(), 2);
    }

    #[test]
    fn selected_entry_uses_visible_index() {
        let entries = vec![entry("archived", true), entry("active", false)];

        assert_eq!(
            selected_entry(&entries, false, 0).map(|entry| entry.session_id.as_str()),
            Some("active")
        );
    }

    #[test]
    fn page_step_leaves_one_line_context() {
        assert_eq!(page_step(8), 7);
        assert_eq!(page_step(0), 0);
    }

    #[test]
    fn palette_inner_rows_caps_suggestions() {
        assert_eq!(palette_inner_rows("", false), 8);
        assert_eq!(palette_inner_rows("", true), 9);
    }

    #[test]
    fn palette_overlay_height_respects_list_reserve() {
        assert_eq!(palette_overlay_height("", false, 6), 3);
        assert_eq!(palette_overlay_height("", false, 20), 10);
    }

    #[test]
    fn palette_lines_include_input_and_help_when_space_allows() {
        let lines = palette_lines("q", false, 80, 3);

        assert_eq!(lines.len(), 3);
        assert!(format!("{:?}", lines[0]).contains("q"));
        assert!(format!("{:?}", lines[2]).contains("Esc close"));
    }

    #[test]
    fn palette_commands_adds_restore_for_archived_selection() {
        assert!(
            !palette_commands(false)
                .iter()
                .any(|command| command.name == "restore")
        );
        assert!(
            palette_commands(true)
                .iter()
                .any(|command| command.name == "restore")
        );
    }
}
