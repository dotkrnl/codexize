//! Footer surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the footer bar (keymap + live status).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FooterView {
    /// Active keybindings for the current context.
    pub keymap: Arc<[KeyBindingView]>,
    /// Live summary from the running agent, if any.
    pub live_agent_message: Option<Arc<str>>,
}

/// One keybinding hint in the footer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyBindingView {
    pub glyph: Arc<str>,
    pub action: Arc<str>,
    pub enabled: bool,
}
