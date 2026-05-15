use super::{App, state::ModelRefreshState, status_line::Severity};
use crate::{
    data::cache,
    data::config::schema::ProviderEntry,
    selection::{CachedModel, CliKind, QuotaError, SubscriptionKind},
};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
const REFRESH_STATUS_TTL: Duration = Duration::from_secs(6);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) fn spawn_refresh(
    cache_dir: PathBuf,
    available_clis: BTreeSet<CliKind>,
    providers: Vec<ProviderEntry>,
) -> mpsc::UnboundedReceiver<(Vec<CachedModel>, Vec<QuotaError>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            let _ = tx.send(
                crate::data::selection_assembly::assemble_models_async(
                    &cache_dir,
                    &available_clis,
                    &providers,
                )
                .await,
            );
        });
    } else {
        let cache_dir_owned = cache_dir;
        let result = crate::data::async_bridge::block_on_io(async move {
            crate::data::selection_assembly::assemble_models_async(
                &cache_dir_owned,
                &available_clis,
                &providers,
            )
            .await
        });
        let _ = tx.send(result.unwrap_or_else(|err| {
            tracing::warn!("model assembly bridge failed: {err}");
            (
                Vec::new(),
                vec![QuotaError {
                    subscription: SubscriptionKind::Direct,
                    message: format!("{err:#}"),
                }],
            )
        }));
    }
    rx
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelsAreaMode {
    #[default]
    FullTable,
    CompactQuota,
}
pub(crate) fn subscription_tag(subscription: SubscriptionKind) -> &'static str {
    match subscription {
        SubscriptionKind::Claude => "claude",
        SubscriptionKind::Codex => "codex",
        SubscriptionKind::Gemini => "gemini",
        SubscriptionKind::Kimi => "kimi",
        SubscriptionKind::OpencodeGo => "opencode",
        SubscriptionKind::Direct => "direct",
    }
}
fn quota_error_summary(errors: &[QuotaError]) -> String {
    let names: Vec<&str> = errors
        .iter()
        .map(|e| subscription_tag(e.subscription))
        .collect();
    match names.as_slice() {
        [] => "model refresh failed".to_string(),
        [single] => format!("model refresh: {single} quota unavailable"),
        many => format!("model refresh: {} quotas unavailable", many.join(", ")),
    }
}
impl App {
    fn available_clis(&self) -> BTreeSet<CliKind> {
        crate::data::acp::AcpConfig::from_config_views(
            &self.config.acp.agents,
            &self.config.acp_install_view(),
        )
        .available_clis()
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
                        self.quota_retry_delay =
                            (self.quota_retry_delay * 2).min(cache::MAX_QUOTA_RETRY_BACKOFF);
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
                        self.quota_retry_delay =
                            (self.quota_retry_delay * 2).min(cache::MAX_QUOTA_RETRY_BACKOFF);
                        self.model_refresh = ModelRefreshState::Idle(Instant::now());
                    }
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.push_status(
                        "model refresh worker exited unexpectedly".to_string(),
                        Severity::Error,
                        REFRESH_STATUS_TTL,
                    );
                    self.quota_retry_delay =
                        (self.quota_retry_delay * 2).min(cache::MAX_QUOTA_RETRY_BACKOFF);
                    self.model_refresh = ModelRefreshState::Idle(Instant::now());
                }
            },
            ModelRefreshState::Idle(refreshed_at) => {
                let due_after = if self.quota_errors.is_empty() {
                    cache::MAX_QUOTA_RETRY_BACKOFF
                } else {
                    self.quota_retry_delay
                };
                if refreshed_at.elapsed() >= due_after {
                    self.model_refresh = ModelRefreshState::Fetching {
                        rx: spawn_refresh(
                            self.paths.cache_root.clone(),
                            self.available_clis(),
                            self.config.providers.value().clone(),
                        ),
                        started_at: Instant::now(),
                    };
                }
            }
        }
    }
    pub(crate) fn force_refresh_models(&mut self) {
        self.model_refresh = ModelRefreshState::Fetching {
            rx: spawn_refresh(
                self.paths.cache_root.clone(),
                self.available_clis(),
                self.config.providers.value().clone(),
            ),
            started_at: Instant::now(),
        };
    }
}
