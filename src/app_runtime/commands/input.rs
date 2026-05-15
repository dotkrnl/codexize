//! Input-editor commands.
//!
//! Replaces today's "raw key into input buffer" passthrough. Char + Paste
//! both collapse into [`InputCommand::InsertText`].
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputCommand {
    InsertText(String),
    Backspace,
    DeleteForward,
    DeleteWordBack,
    DeleteWordForward,
    MoveCursor(CursorMove),
    Submit,
    Cancel,
    /// Replace the buffer wholesale, used by frontends that emit a finished
    /// line of text (e.g. paste of a multi-line value).
    ReplaceBuffer(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorMove {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    LineUp,
    LineDown,
}
