use super::*;
use crate::app_runtime::commands::{ModalAction, ModalCommand, PaletteCommand, SessionCommand};
use crate::app_runtime::views::modal::StageId;

#[test]
fn restore_terminal_after_failed_start_is_idempotent() {
    // Cleanup must complete without panicking even when raw mode is not
    // engaged and no alternate screen has been entered, so the failure
    // path of `start()` cannot make a bad situation worse.
    restore_terminal_after_failed_start();
    restore_terminal_after_failed_start();
    // After cleanup, raw mode must not be left engaged.
    assert!(
        !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false),
        "raw mode must be disabled after the cleanup helper runs"
    );
}

#[test]
fn quit_running_agent_modal_keys_become_domain_commands() {
    let mut view = AppView::empty("ui-command-test");
    view.modal = Some(ModalKind::QuitRunningAgent);

    let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let cancel = Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

    assert_eq!(
        command_from_event(enter, &view),
        Some(AppCommand::Session(
            view.session_id.clone(),
            SessionCommand::Modal(ModalCommand::Confirm)
        ))
    );
    assert_eq!(
        command_from_event(cancel, &view),
        Some(AppCommand::Session(
            view.session_id.clone(),
            SessionCommand::Modal(ModalCommand::Cancel)
        ))
    );
}

#[test]
fn stage_error_retry_key_becomes_modal_retry_action() {
    let mut view = AppView::empty("ui-command-test");
    view.modal = Some(ModalKind::StageError(StageId::FinalValidation));

    let retry = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

    assert_eq!(
        command_from_event(retry, &view),
        Some(AppCommand::Session(
            view.session_id.clone(),
            SessionCommand::Modal(ModalCommand::Action(ModalAction::RetryStage(
                StageId::FinalValidation
            )))
        ))
    );
}

#[test]
fn tab_keys_survive_terminal_command_bridge() {
    let view = AppView::empty("ui-command-test");

    let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let back_tab = Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));

    assert_eq!(
        command_from_event(tab, &view),
        Some(AppCommand::Session(
            view.session_id.clone(),
            SessionCommand::Palette(PaletteCommand::AcceptGhost)
        ))
    );
    assert_eq!(command_from_event(back_tab, &view), None);
}

#[test]
fn wrap_text_preserves_explicit_newlines() {
    let lines = wrap_text("first\nsecond", 100);
    assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
}

#[test]
fn wrap_text_keeps_trailing_blank_lines() {
    let lines = wrap_text("a\n\n\nb", 5);
    assert_eq!(
        lines,
        vec![
            "a".to_string(),
            String::new(),
            String::new(),
            "b".to_string()
        ]
    );
}

#[test]
fn wrap_text_short_input_fits_one_line() {
    assert_eq!(
        wrap_text("hello world", 80),
        vec!["hello world".to_string()]
    );
}

#[test]
fn wrap_text_breaks_on_word_boundary_when_possible() {
    let lines = wrap_text("alpha beta gamma", 10);
    // "alpha beta" is 10 chars (incl. trailing space "alpha beta " = 11),
    // so the wrap point should split at the first space that overflows.
    assert!(
        lines.iter().all(|l| l.chars().count() <= 10),
        "all wrapped lines must fit width=10: {lines:?}"
    );
    assert_eq!(lines.len(), 2);
}

#[test]
fn wrap_text_hard_breaks_overlong_word() {
    // A single word longer than width must be split mid-word into chunks
    // each at most `width` chars wide.
    let lines = wrap_text("aaaaaaaaaaaaaaaaa", 5);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "aaaaa");
    assert_eq!(lines[1], "aaaaa");
    assert_eq!(lines[2], "aaaaa");
    assert_eq!(lines[3], "aa");
}

#[test]
fn wrap_text_handles_unicode_by_char_count() {
    // Non-ASCII chars count as 1 char each (not bytes).
    let lines = wrap_text("héllo wörld", 5);
    assert!(
        lines.iter().all(|l| l.chars().count() <= 5),
        "lines must not exceed width by char count: {lines:?}"
    );
}
