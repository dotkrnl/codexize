use crate::selection::{ModelStatus, QuotaError};
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug)]
pub(super) enum ModelRefreshState {
    Fetching {
        rx: mpsc::Receiver<(Vec<ModelStatus>, Vec<QuotaError>)>,
        started_at: Instant,
    },
    Idle(Instant),
}
