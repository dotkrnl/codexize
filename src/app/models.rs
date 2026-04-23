use crate::{
    cache,
    selection::{ModelStatus, QuotaError, VendorKind},
};
use ratatui::style::Color;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use super::{App, state::ModelRefreshState};

pub(super) fn spawn_refresh() -> mpsc::Receiver<(Vec<ModelStatus>, Vec<QuotaError>)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(crate::selection::load_all_models());
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

impl App {
    pub(super) fn refresh_models_if_due(&mut self) {
        match &self.model_refresh {
            ModelRefreshState::Fetching { rx, started_at } => {
                match rx.try_recv() {
                    Ok((models, errors)) => {
                        if !models.is_empty() {
                            self.models = models;
                            let _ = cache::save(&self.models, &errors);
                        }
                        if errors.is_empty() {
                            self.quota_retry_delay = Duration::from_secs(60);
                        } else {
                            self.quota_retry_delay =
                                (self.quota_retry_delay * 2).min(cache::TTL);
                        }
                        self.quota_errors = errors;
                        self.model_refresh = ModelRefreshState::Idle(Instant::now());
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if started_at.elapsed() >= Duration::from_secs(60) {
                            self.quota_retry_delay =
                                (self.quota_retry_delay * 2).min(cache::TTL);
                            self.model_refresh = ModelRefreshState::Idle(Instant::now());
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.quota_retry_delay =
                            (self.quota_retry_delay * 2).min(cache::TTL);
                        self.model_refresh = ModelRefreshState::Idle(Instant::now());
                    }
                }
            }
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
