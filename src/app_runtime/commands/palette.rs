//! Command-palette overlay commands.
use super::input::InputCommand;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaletteCommand {
    /// Open the palette browser.
    Open,
    /// Close the palette, optionally restoring input focus.
    Close { restore_input_focus: bool },
    /// Submit the current palette buffer.
    Submit,
    /// Accept the ghost completion for the current prefix.
    AcceptGhost,
    /// Edit the palette buffer (insert text, backspace, cursor moves).
    Edit(InputCommand),
}
