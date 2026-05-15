//! Palette surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the command palette.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PaletteView {
    pub is_open: bool,
    pub input_buffer: Arc<str>,
    pub ghost_text: Option<Arc<str>>,
    pub filtered_commands: Arc<[PaletteCommandView]>,
    pub selected_index: usize,
    pub prompt: Option<Arc<str>>,
}

/// A command available in the palette browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaletteCommandView {
    pub name: Arc<str>,
    pub help: Arc<str>,
    pub key_hint: Option<Arc<str>>,
}

pub(crate) use crate::app::palette::{
    MatchResult, PaletteCommand, PaletteState, filter, ghost_completion, resolve, suggestion_text,
};
