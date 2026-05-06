use crate::selection::{CachedModel, QuotaError};
use std::time::Instant;
use tokio::sync::mpsc;
#[derive(Debug)]
pub(crate) enum ModelRefreshState {
    Fetching {
        rx: mpsc::UnboundedReceiver<(Vec<CachedModel>, Vec<QuotaError>)>,
        started_at: Instant,
    },
    Idle(Instant),
}
