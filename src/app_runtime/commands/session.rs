//! Per-session command grouping.
use super::chat::ChatCommand;
use super::config_panel::ConfigPanelCommand;
use super::input::InputCommand;
use super::modal::ModalCommand;
use super::modes::ModesCommand;
use super::palette::PaletteCommand;
use super::picker::PickerCommand;
use super::sheet::SheetCommand;
use super::split::SplitCommand;
use super::stage::StageCommand;
use super::status::StatusCommand;
use super::tree::TreeCommand;
use serde::{Deserialize, Serialize};

/// Commands scoped to a particular session, routed by `SessionId` at the
/// top-level [`crate::app_runtime::AppCommand::Session`] variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionCommand {
    Tree(TreeCommand),
    Chat(ChatCommand),
    Palette(PaletteCommand),
    Input(InputCommand),
    Modal(ModalCommand),
    Stage(StageCommand),
    Modes(ModesCommand),
    Split(SplitCommand),
    Picker(PickerCommand),
    ConfigPanel(ConfigPanelCommand),
    Sheet(SheetCommand),
    Status(StatusCommand),
    /// Submit a single pre-baked input line. The runtime treats it as
    /// "stuff this in the input buffer and press Enter". Equivalent to
    /// `Palette(Edit(ReplaceBuffer(text))) + Palette(Submit)` for the
    /// palette buffer, but kept as a discrete variant because the legacy
    /// `SubmitInput` command shape is widely used in tests and other
    /// non-typed entry points.
    SubmitInput {
        text: String,
    },
    /// Run a named palette command directly (skipping the buffer round
    /// trip). Useful for non-key surfaces (mouse, headless scripted
    /// flows).
    PaletteCommand {
        name: String,
        args: String,
    },
}
