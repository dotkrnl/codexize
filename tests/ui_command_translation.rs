use codexize::app_runtime::{AppCommand, UiKey, UiKeyCode};
use codexize::ui::tui::command_from_event;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[test]
fn key_press_event_becomes_ui_neutral_command() {
    let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_eq!(
        command_from_event(event),
        Some(AppCommand::KeyPress(UiKey {
            code: UiKeyCode::Char('c'),
            ctrl: true,
            alt: false,
        }))
    );
}

#[test]
fn key_release_event_is_ignored_by_ui_translation() {
    let event = Event::Key(KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: crossterm::event::KeyEventState::NONE,
    });

    assert_eq!(command_from_event(event), None);
}

#[test]
fn paste_event_becomes_submit_input_command() {
    assert_eq!(
        command_from_event(Event::Paste("hello".to_string())),
        Some(AppCommand::PasteInput {
            text: "hello".to_string(),
        })
    );
}
