//! Bottom sheet surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the bottom sheet (transient overlays).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SheetView {
    pub is_open: bool,
    pub title: Arc<str>,
    pub content_lines: Arc<[Arc<str>]>,
    pub controls_line: Arc<str>,
}
