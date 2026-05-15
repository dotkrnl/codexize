//! TUI-shaped key types used internally by terminal input translation and
//! legacy focus-local handlers.
//!
//! These are not part of the runtime frontend seam. They live outside
//! `app/` and `ui/` so neither side has to import the other just to share
//! the temporary key adapter shape during the cutover.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiKey {
    pub code: UiKeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl UiKey {
    /// Synthetic key with no modifiers (the common case for command-to-key
    /// bridging in the transitional typed-command handlers).
    pub(crate) fn new(code: UiKeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiKeyCode {
    Esc,
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Char(char),
    Unknown,
}

/// Translate a TUI-shaped key into the corresponding config-panel command.
///
/// This bridges legacy key handlers into the typed `ConfigPanelCommand`
/// surface without making `ui/` import from `app/`.
pub(crate) fn config_panel_key_to_command(
    key: UiKey,
) -> crate::app_runtime::commands::ConfigPanelCommand {
    use crate::app_runtime::commands::{ConfigPanelCommand, CursorMove, InputCommand};

    if key.ctrl {
        return match key.code {
            UiKeyCode::Char('s') => ConfigPanelCommand::Save,
            UiKeyCode::Char('c') => ConfigPanelCommand::Cancel,
            UiKeyCode::Char('d') => ConfigPanelCommand::HalfPageDown,
            UiKeyCode::Char('u') => ConfigPanelCommand::HalfPageUp,
            UiKeyCode::Char('i') => ConfigPanelCommand::NextSection,
            UiKeyCode::Char('h') => ConfigPanelCommand::Edit(InputCommand::Backspace),
            UiKeyCode::Char(c) => ConfigPanelCommand::Edit(InputCommand::InsertText(c.to_string())),
            _ => ConfigPanelCommand::Edit(InputCommand::InsertText(String::new())),
        };
    }

    match key.code {
        UiKeyCode::Esc | UiKeyCode::Char('q') => ConfigPanelCommand::Close,
        UiKeyCode::Up | UiKeyCode::Char('k') => ConfigPanelCommand::MoveUp,
        UiKeyCode::Down | UiKeyCode::Char('j') => ConfigPanelCommand::MoveDown,
        UiKeyCode::Left | UiKeyCode::Char('h') => ConfigPanelCommand::PrevSection,
        UiKeyCode::Right | UiKeyCode::Char('l') => ConfigPanelCommand::NextSection,
        UiKeyCode::Enter => ConfigPanelCommand::Activate,
        UiKeyCode::Char(' ') => ConfigPanelCommand::Toggle,
        UiKeyCode::Char('n') => ConfigPanelCommand::AddProvider,
        UiKeyCode::Char('x') => ConfigPanelCommand::DeleteProvider,
        UiKeyCode::Char('d') => ConfigPanelCommand::DeleteEntry,
        UiKeyCode::Char('r') => ConfigPanelCommand::ToggleSecretReveal,
        UiKeyCode::Char('R') => ConfigPanelCommand::RemoveSavedSecret,
        UiKeyCode::Tab => ConfigPanelCommand::NextSection,
        UiKeyCode::BackTab => ConfigPanelCommand::PrevSection,
        UiKeyCode::Char('[') => ConfigPanelCommand::PrevSectionBracket,
        UiKeyCode::Char(']') => ConfigPanelCommand::NextSectionBracket,
        UiKeyCode::Char('g') => ConfigPanelCommand::JumpTop,
        UiKeyCode::Char('G') => ConfigPanelCommand::JumpBottom,
        UiKeyCode::PageDown => ConfigPanelCommand::HalfPageDown,
        UiKeyCode::PageUp => ConfigPanelCommand::HalfPageUp,
        UiKeyCode::Char('/') => ConfigPanelCommand::Edit(InputCommand::InsertText("/".to_string())),
        UiKeyCode::Backspace => ConfigPanelCommand::Edit(InputCommand::Backspace),
        UiKeyCode::Delete => ConfigPanelCommand::Edit(InputCommand::DeleteForward),
        UiKeyCode::Home => ConfigPanelCommand::Edit(InputCommand::MoveCursor(CursorMove::Home)),
        UiKeyCode::End => ConfigPanelCommand::Edit(InputCommand::MoveCursor(CursorMove::End)),
        UiKeyCode::Char(c) => ConfigPanelCommand::Edit(InputCommand::InsertText(c.to_string())),
        _ => ConfigPanelCommand::Edit(InputCommand::InsertText(String::new())),
    }
}
