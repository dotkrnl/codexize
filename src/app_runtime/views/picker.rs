//! Session picker surface view.
use crate::logic::pipeline::Stage;
use serde::Serialize;
use std::sync::Arc;

/// View projection for the session picker (startup screen).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PickerView {
    pub entries: Arc<[PickerEntryView]>,
    pub selected_index: usize,
    pub is_input_mode: bool,
    pub input_buffer: Arc<str>,
}

/// One session entry in the picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PickerEntryView {
    pub session_id: Arc<str>,
    pub summary: Arc<str>,
    pub stage: Stage,
    pub is_archived: bool,
    /// Last modified timestamp (e.g. RFC3339).
    pub last_modified: Arc<str>,
}
