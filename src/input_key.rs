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
