//! Per-surface typed operator-intent commands.
//!
//! `AppCommand` (re-exported at the crate root) groups every operator
//! action into [`GlobalCommand`], [`ShellCommand`], and per-session
//! [`SessionCommand`]s, with one file per spec-pinned surface mirroring
//! `app_runtime/views/`.
//!
//! Every sub-command derives `Clone + PartialEq + Eq + Debug +
//! serde::Serialize + serde::Deserialize` so a non-terminal frontend
//! (`HeadlessFrontend`) can parse stdin lines into the same enum tree
//! that the TUI produces from its [`crate::ui::input_translation`]
//! step.
pub mod chat;
pub mod config_panel;
pub mod global;
pub mod input;
pub mod modal;
pub mod modes;
pub mod palette;
pub mod picker;
pub mod session;
pub mod sheet;
pub mod shell;
pub mod split;
pub mod stage;
pub mod status;
pub mod tree;

pub use chat::ChatCommand;
pub use config_panel::ConfigPanelCommand;
pub use global::GlobalCommand;
pub use input::{CursorMove, InputCommand};
pub use modal::{ModalAction, ModalCommand};
pub use modes::ModesCommand;
pub use palette::PaletteCommand;
pub use picker::PickerCommand;
pub use session::SessionCommand;
pub use sheet::SheetCommand;
pub use shell::ShellCommand;
pub use split::SplitCommand;
pub use stage::StageCommand;
pub use status::StatusCommand;
pub use tree::TreeCommand;
