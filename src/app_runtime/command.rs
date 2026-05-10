//! Operator-intent commands the UI emits back to the runtime.
//!
//! `AppCommand` is the only way a UI expresses operator intent. Commands
//! are domain-level (approve a plan, retry a stage, quit, …) — never raw
//! terminal key codes. The TUI translates `KeyEvent`s into `AppCommand`s
//! inside `ui/`; a future web UI translates HTTP requests into the same
//! enum. The runtime routes the command into [`crate::logic`] and any
//! resulting [`crate::data::events::DataRequest`].
//!
//! The production TUI now translates a subset of `KeyEvent`s into
//! `AppCommand`s before they reach the [`crate::app::App`] event pump
//! (e.g. Esc on the quit-running-agent modal becomes
//! [`AppCommand::CancelModal`]). Remaining focus-local key handling still
//! lives in `app/`.
use super::view::StageId;
/// UI-neutral key action emitted by terminal input collection.
///
/// This intentionally mirrors only operator-visible key intent, not the
/// concrete crossterm event type. `App` resolves these against its current
/// focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiKey {
    pub code: UiKeyCode,
    pub ctrl: bool,
    pub alt: bool,
}
/// Terminal-independent key identity used inside [`AppCommand::KeyPress`].
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
    Char(char),
    Unknown,
}
/// Domain-level operator intent. Variants intentionally avoid encoding
/// terminal-specific input (keysyms, scroll deltas in pixels, …) so the
/// same enum can drive a non-terminal UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// UI-neutral key action. Kept as an incremental bridge so production
    /// input collection leaves `app` before every focus-local command is
    /// split into a narrower domain variant.
    KeyPress(UiKey),
    /// Paste text into whichever input surface currently owns editing.
    PasteInput { text: String },
    /// Quit the application. Subject to the runtime's pending-agent and
    /// confirmation gating.
    Quit,
    /// Approve a pending review modal (spec or plan review).
    Approve,
    /// Reject a pending review modal, requesting revisions.
    Reject,
    /// Open the command palette overlay.
    OpenPalette,
    /// Close the command palette overlay.
    ClosePalette,
    /// Toggle YOLO mode for newly launched runs.
    ToggleYolo,
    /// Toggle Cheap mode for newly launched runs.
    ToggleCheap,
    /// Skip directly to implementation, bypassing planning review.
    SkipToImpl,
    /// Retry the currently running stage.
    RetryStage(StageId),
    /// Stop the currently running agent without retry.
    StopAgent,
    /// Move tree focus by `delta` rows (negative = up, positive = down).
    MoveFocus { delta: isize },
    /// Toggle expand/collapse on the focused tree node.
    ToggleExpand,
    /// Submit the operator's input buffer as a message to the active run.
    SubmitInput { text: String },
    /// Open the split transcript panel for the focused run.
    OpenSplit,
    /// Close the split transcript panel.
    CloseSplit,
    /// Dismiss any active status-line message.
    DismissStatus,
    /// Confirm a destructive action (e.g. quit-while-running, git guard).
    ConfirmModal,
    /// Cancel an active modal without taking the destructive action.
    CancelModal,
}
#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
