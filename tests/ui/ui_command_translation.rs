use codexize::app_runtime::{AppCommand, AppView, ModalKind, UiKey, UiKeyCode};
use codexize::ui::tui::command_from_event;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

fn idle_view() -> AppView {
    AppView::empty("ui-cmd-test")
}

fn quit_modal_view() -> AppView {
    let mut view = AppView::empty("ui-cmd-test");
    view.modal = Some(ModalKind::QuitRunningAgent);
    view
}

#[test]
fn key_press_event_becomes_ui_neutral_command() {
    let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_eq!(
        command_from_event(event, &idle_view()),
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

    assert_eq!(command_from_event(event, &idle_view()), None);
}

#[test]
fn paste_event_becomes_submit_input_command() {
    assert_eq!(
        command_from_event(Event::Paste("hello".to_string()), &idle_view()),
        Some(AppCommand::PasteInput {
            text: "hello".to_string(),
        })
    );
}

#[test]
fn esc_on_quit_modal_translates_to_cancel_modal_command() {
    let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(
        command_from_event(event, &quit_modal_view()),
        Some(AppCommand::CancelModal),
    );
}

#[test]
fn esc_outside_quit_modal_remains_a_keypress() {
    let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(
        command_from_event(event, &idle_view()),
        Some(AppCommand::KeyPress(UiKey {
            code: UiKeyCode::Esc,
            ctrl: false,
            alt: false,
        }))
    );
}
