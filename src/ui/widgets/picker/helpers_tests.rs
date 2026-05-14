use super::*;
use crate::state::{Modes, Stage};
use std::time::SystemTime;

fn entry(id: &str, archived: bool) -> SessionEntry {
    SessionEntry {
        session_id: id.to_string(),
        idea_summary: id.to_string(),
        current_stage: Stage::IdeaInput,
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
    assert_eq!(palette_inner_rows("", false), 9);
    assert_eq!(palette_inner_rows("", true), 10);
}

#[test]
fn palette_overlay_height_respects_list_reserve() {
    assert_eq!(palette_overlay_height("", false, 6), 3);
    assert_eq!(palette_overlay_height("", false, 20), 11);
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
