use crate::selection::{CachedModel, QuotaError};
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug)]
pub(super) enum ModelRefreshState {
    Fetching {
        rx: mpsc::Receiver<(Vec<CachedModel>, Vec<QuotaError>)>,
        started_at: Instant,
    },
    Idle(Instant),
}
