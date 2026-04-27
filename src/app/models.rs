use crate::{
    cache,
    selection::{self, CachedModel, QuotaError, VendorKind, ranking::build_version_index},
};
use ratatui::style::Color;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use super::{App, state::ModelRefreshState, status_line::Severity};

const REFRESH_STATUS_TTL: Duration = Duration::from_secs(6);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn spawn_refresh() -> mpsc::Receiver<(Vec<CachedModel>, Vec<QuotaError>)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(selection::assemble::assemble_models());
    });
    rx
}

pub(super) fn vendor_tag(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude",
        VendorKind::Codex => "codex",
        VendorKind::Gemini => "gemini",
        VendorKind::Kimi => "kimi",
    }
}

pub(super) fn vendor_color(vendor: VendorKind) -> Color {
    match vendor {
        VendorKind::Claude => Color::Magenta,
        VendorKind::Codex => Color::Green,
        VendorKind::Gemini => Color::Blue,
        VendorKind::Kimi => Color::Yellow,
    }
}

pub(super) fn vendor_prefix(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude-",
        VendorKind::Codex => "gpt-",
        VendorKind::Gemini => "gemini-",
        VendorKind::Kimi => "kimi-",
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
    pub(super) fn set_models(&mut self, models: Vec<CachedModel>) {
        self.versions = build_version_index(&models);
        self.models = models;
    }

    pub(super) fn refresh_models_if_due(&mut self) {
        match &self.model_refresh {
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
                Err(std::sync::mpsc::TryRecvError::Empty) => {
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
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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
                        rx: spawn_refresh(),
                        started_at: Instant::now(),
                    };
                }
            }
        }
    }

    pub(super) fn force_refresh_models(&mut self) {
        self.model_refresh = ModelRefreshState::Fetching {
            rx: spawn_refresh(),
            started_at: Instant::now(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_error_summary_single_vendor() {
        let errors = vec![QuotaError {
            vendor: VendorKind::Claude,
            message: "429".to_string(),
        }];
        assert_eq!(
            quota_error_summary(&errors),
            "model refresh: claude quota unavailable"
        );
    }

    #[test]
    fn quota_error_summary_multiple_vendors() {
        let errors = vec![
            QuotaError {
                vendor: VendorKind::Claude,
                message: "429".to_string(),
            },
            QuotaError {
                vendor: VendorKind::Codex,
                message: "503".to_string(),
            },
        ];
        assert_eq!(
            quota_error_summary(&errors),
            "model refresh: claude, codex quotas unavailable"
        );
    }
}
