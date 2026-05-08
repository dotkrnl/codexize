use super::{App, state::ModelRefreshState, status_line::Severity};
use crate::{
    cache,
    selection::{CachedModel, QuotaError, VendorKind},
};
use ratatui::style::Color;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
const REFRESH_STATUS_TTL: Duration = Duration::from_secs(6);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) fn spawn_refresh(
    cache_dir: PathBuf,
    available_vendors: BTreeSet<VendorKind>,
) -> mpsc::UnboundedReceiver<(Vec<CachedModel>, Vec<QuotaError>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            let _ = tx.send(
                crate::data::selection_assembly::assemble_models_async(
                    &cache_dir,
                    &available_vendors,
                )
                .await,
            );
        });
    } else {
        let cache_dir_owned = cache_dir;
        let _ = tx.send(crate::data::async_bridge::block_on_io(async move {
            crate::data::selection_assembly::assemble_models_async(
                &cache_dir_owned,
                &available_vendors,
            )
            .await
        }));
    }
    rx
}
pub(crate) fn vendor_tag(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude",
        VendorKind::Codex => "codex",
        VendorKind::Gemini => "gemini",
        VendorKind::Kimi => "kimi",
        VendorKind::Opencode => "opencode",
    }
}
pub(crate) fn vendor_color(vendor: VendorKind) -> Color {
    match vendor {
        VendorKind::Claude => Color::Magenta,
        VendorKind::Codex => Color::Green,
        VendorKind::Gemini => Color::Blue,
        VendorKind::Kimi => Color::Yellow,
        VendorKind::Opencode => Color::Cyan,
    }
}
pub(crate) fn vendor_prefix(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude-",
        VendorKind::Codex => "gpt-",
        VendorKind::Gemini => "gemini-",
        VendorKind::Kimi => "kimi-",
        VendorKind::Opencode => "",
    }
}
fn quota_error_summary(errors: &[QuotaError]) -> String {
    let names: Vec<&str> = errors.iter().map(|e| vendor_tag(e.vendor)).collect();
    match names.as_slice() {
        [] => "model refresh failed".to_string(),
        [single] => format!("model refresh: {single} quota unavailable"),
        many => format!("model refresh: {} quotas unavailable", many.join(", ")),
    }
}
impl App {
    fn available_vendors(&self) -> BTreeSet<VendorKind> {
        crate::acp::AcpConfig::from_config_views(
            &self.config.acp.agents,
            &self.config.acp_install_view(),
        )
        .available_vendors()
    }
    pub(crate) fn set_models(&mut self, models: Vec<CachedModel>) {
        self.models = models;
    }
    pub(crate) fn refresh_models_if_due(&mut self) {
        match &mut self.model_refresh {
            ModelRefreshState::Fetching { rx, started_at } => match rx.try_recv() {
                Ok((models, errors)) => {
                    if !models.is_empty() {
                        self.set_models(models);
                    }
                    if errors.is_empty() {
                        self.quota_retry_delay = Duration::from_secs(60);
                    } else {
                        let summary = quota_error_summary(&errors);
                        self.push_status(summary, Severity::Warn, REFRESH_STATUS_TTL);
                        self.quota_retry_delay = (self.quota_retry_delay * 2).min(cache::TTL);
                    }
                    self.quota_errors = errors;
                    self.model_refresh = ModelRefreshState::Idle(Instant::now());
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    if started_at.elapsed() >= REFRESH_TIMEOUT {
                        self.push_status(
                            "model refresh timed out — retrying".to_string(),
                            Severity::Warn,
                            REFRESH_STATUS_TTL,
                        );
                        self.quota_retry_delay = (self.quota_retry_delay * 2).min(cache::TTL);
                        self.model_refresh = ModelRefreshState::Idle(Instant::now());
                    }
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.push_status(
                        "model refresh worker exited unexpectedly".to_string(),
                        Severity::Error,
                        REFRESH_STATUS_TTL,
                    );
                    self.quota_retry_delay = (self.quota_retry_delay * 2).min(cache::TTL);
                    self.model_refresh = ModelRefreshState::Idle(Instant::now());
                }
            },
            ModelRefreshState::Idle(refreshed_at) => {
                let due_after = if self.quota_errors.is_empty() {
                    cache::TTL
                } else {
                    self.quota_retry_delay
                };
                if refreshed_at.elapsed() >= due_after {
                    self.model_refresh = ModelRefreshState::Fetching {
                        rx: spawn_refresh(self.paths.cache_root.clone(), self.available_vendors()),
                        started_at: Instant::now(),
                    };
                }
            }
        }
    }
    pub(crate) fn force_refresh_models(&mut self) {
        self.model_refresh = ModelRefreshState::Fetching {
            rx: spawn_refresh(self.paths.cache_root.clone(), self.available_vendors()),
            started_at: Instant::now(),
        };
    }
}
#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
