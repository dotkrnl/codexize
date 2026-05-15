//! Modal-overlay commands.
//!
//! The TUI translates per-modal key bindings (Enter, Esc, single-letter
//! action keys) into one of these variants. The runtime dispatches based on
//! the modal kind that is currently active in the session view.
use crate::app_runtime::views::modal::StageId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModalCommand {
    /// Primary affirmative answer (Enter / Y).
    Confirm,
    /// Primary negative answer / dismissal (Esc / N / Q).
    Cancel,
    /// Modal-specific action key.
    Action(ModalAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModalAction {
    /// Stage-error modal: retry the failed stage (`r` / Enter).
    RetryStage(StageId),
    /// Stage-error modal on Brainstorm: edit the idea (`e`).
    EditIdea,
    /// Stage-error or dreaming-decision modal: skip dreaming (`s`).
    SkipDreaming,
    /// Final-validation-blocked modal: accept "force ship" (`f` / Enter).
    ForceShip,
    /// Final-validation-blocked modal: recover via builder (`r`).
    RecoverFromBlock,
    /// Git-guard modal: accept the reset (`r` / Enter).
    GuardReset,
    /// Git-guard modal: keep the working tree (`k`).
    GuardKeep,
    /// Skip-to-impl modal: accept (`y` / Enter).
    AcceptSkipToImpl,
    /// Skip-to-impl modal: decline (`n`).
    DeclineSkipToImpl,
    /// Dreaming-decision modal: run dreaming (`r` / Enter).
    RunDreaming,
    /// Spec/Plan-review-paused modal: open palette (`:`).
    OpenPaletteFromPaused,
    /// Interactive-exit-prompt modal: any printable char dismisses prompt
    /// and forwards the inserted text into the input buffer.
    InteractiveExitInsertChar(char),
}
