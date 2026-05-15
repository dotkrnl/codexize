//! TUI-shaped key types used internally by `app/` handlers and tests.
//!
//! These are NOT part of the runtime seam. They live under `app/` so the
//! focus-local key handlers can keep their existing key-matching shape
//! while the seam itself carries strictly typed `AppCommand`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiKey {
    pub code: UiKeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKeyCode {
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
