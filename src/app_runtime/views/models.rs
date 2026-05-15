//! Models surface view.
use serde::Serialize;
use std::sync::Arc;

/// View projection for available AI models and their refresh status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModelsView {
    pub models: Arc<[ModelView]>,
    pub refresh_state: ModelRefreshViewStatus,
}

/// One available AI model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelView {
    pub name: Arc<str>,
    pub provider: Arc<str>,
    pub subscription: Arc<str>,
    pub is_available: bool,
}

/// Status of the model refresh process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ModelRefreshViewStatus {
    #[default]
    Idle,
    Refreshing,
    Failed,
}

pub use crate::app::models::ModelsAreaMode;
