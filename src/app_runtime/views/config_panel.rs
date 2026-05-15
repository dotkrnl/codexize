//! Configuration panel surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the in-app configuration editor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConfigPanelView {
    pub is_open: bool,
    pub sections: Arc<[ConfigSectionView]>,
    pub selected_section_index: usize,
    pub selected_field_index: usize,
}

/// A group of related configuration fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigSectionView {
    pub title: Arc<str>,
    pub fields: Arc<[ConfigFieldView]>,
}

/// One editable configuration field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigFieldView {
    pub label: Arc<str>,
    pub value: Arc<str>,
    pub description: Arc<str>,
    pub is_secret: bool,
}
