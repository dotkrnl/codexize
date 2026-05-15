//! Configuration panel surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for the in-app configuration editor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConfigPanelView {
    pub is_open: bool,
    pub is_searching: bool,
    pub is_editing: bool,
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

pub(crate) mod providers {
    #[cfg(test)]
    pub(crate) use crate::app::config_panel::providers::all_vendors;
    pub(crate) use crate::app::config_panel::providers::{
        AddProviderField, ProvidersEditor, ProvidersLine, get_lines,
    };
}

#[cfg(test)]
pub(crate) use crate::app::config_panel::field_index_for_test;
pub(crate) use crate::app::config_panel::{
    ConfigPanelState, ConflictBanner, EXIT_OPTIONS, Editing, FIELDS, FieldKind, FieldMeta,
    PROVIDER_TOGGLES, PanelOutcome, ProviderToggle, SECTIONS, SectionLookup, ToggleField,
    is_providers_section, lookup_section, section_title, terminal_too_narrow_message,
};
