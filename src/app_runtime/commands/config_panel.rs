//! Config-panel commands.
//!
//! The config panel is a navigable per-field editor with a wide keymap
//! surface; the variants below mirror each operator action surfaced by the
//! current key handler.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigPanelCommand {
    /// Open the panel, optionally at a specific section.
    Open { section: Option<String> },
    /// Close without saving (Esc / `q`).
    Close,
    /// Save and apply the pending edits (Ctrl-s).
    Save,
    /// Cancel via Ctrl-c (separate from Close so quirks can be preserved).
    Cancel,
    /// Move selected row up.
    MoveUp,
    /// Move selected row down.
    MoveDown,
    /// Decrement / move-left on the selected field.
    DecrementValue,
    /// Increment / move-right on the selected field.
    IncrementValue,
    /// Activate the selected field (Enter).
    Activate,
    /// Toggle the selected field (Space).
    Toggle,
    /// Add a new entry in the active providers section (`n`).
    AddProvider,
    /// Delete the selected provider (`x`).
    DeleteProvider,
    /// Half-page up (Ctrl-u).
    HalfPageUp,
    /// Half-page down (Ctrl-d).
    HalfPageDown,
    /// Page up to the top (`g`).
    JumpTop,
    /// Page down to the bottom (`G`).
    JumpBottom,
    /// Tab to next section.
    NextSection,
    /// BackTab to previous section.
    PrevSection,
    /// Previous section bracket (`[`).
    PrevSectionBracket,
    /// Next section bracket (`]`).
    NextSectionBracket,
    /// Jump to help / docs (Ctrl-i).
    JumpHelp,
    /// Delete entry (`d`).
    DeleteEntry,
    /// Toggle secret reveal on a sensitive field (`r`).
    ToggleSecretReveal,
    /// Remove the saved secret value (`R`).
    RemoveSavedSecret,
}
