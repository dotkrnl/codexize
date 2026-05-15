//! Watchdog surface view.
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

/// View projection for the run liveness watchdog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WatchdogView {
    /// Active alerts (warnings or kills) for monitored runs.
    pub alerts: Arc<[WatchdogAlertView]>,
}

/// One watchdog alert for a specific run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchdogAlertView {
    pub run_id: u64,
    pub idle_duration: Duration,
    pub is_warning: bool,
    pub is_kill: bool,
}
