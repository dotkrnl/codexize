use crate::selection::{ModelStatus, QuotaError, VendorKind};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Serialize, Deserialize, Default)]
struct CachedModels {
    saved_at: u64,
    models: Vec<CachedModel>,
    #[serde(default)]
    quota_errors: Vec<CachedQuotaError>,
}

#[derive(Serialize, Deserialize)]
struct CachedQuotaError {
    vendor: String,
    message: String,
}

#[derive(Serialize, Deserialize)]
struct CachedModel {
    vendor: String,
    name: String,
    stupid_level: Option<u8>,
    quota_percent: Option<u8>,
    idea_rank: u8,
    planning_rank: u8,
    build_rank: u8,
    review_rank: u8,
    #[serde(default)]
    idea_weight: f64,
    #[serde(default)]
    planning_weight: f64,
    #[serde(default)]
    build_weight: f64,
    #[serde(default)]
    review_weight: f64,
}

fn cache_path() -> PathBuf {
    let base = dirs_home().unwrap_or_else(|| PathBuf::from("."));
    base.join(".codexize").join("cache").join("models.json")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn load() -> Option<(Vec<ModelStatus>, Vec<QuotaError>, bool)> {
    let path = cache_path();
    let text = fs::read_to_string(&path).ok()?;
    let cached: CachedModels = serde_json::from_str(&text).ok()?;

    let age = now_secs().saturating_sub(cached.saved_at);
    let expired = age >= TTL.as_secs();

    let models = cached
        .models
        .into_iter()
        .filter_map(|m| {
            let vendor = parse_vendor(&m.vendor)?;
            Some(ModelStatus {
                vendor,
                name: m.name,
                stupid_level: m.stupid_level,
                quota_percent: m.quota_percent,
                idea_rank: m.idea_rank,
                planning_rank: m.planning_rank,
                build_rank: m.build_rank,
                review_rank: m.review_rank,
                idea_weight: m.idea_weight,
                planning_weight: m.planning_weight,
                build_weight: m.build_weight,
                review_weight: m.review_weight,
            })
        })
        .collect::<Vec<_>>();

    if models.is_empty() {
        return None;
    }

    let errors = cached
        .quota_errors
        .into_iter()
        .filter_map(|e| {
            let vendor = parse_vendor(&e.vendor)?;
            Some(QuotaError { vendor, message: e.message })
        })
        .collect();

    Some((models, errors, expired))
}

pub fn save(models: &[ModelStatus], errors: &[QuotaError]) -> Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create cache directory")?;
    }

    let cached = CachedModels {
        saved_at: now_secs(),
        models: models
            .iter()
            .map(|m| CachedModel {
                vendor: vendor_str(m.vendor).to_string(),
                name: m.name.clone(),
                stupid_level: m.stupid_level,
                quota_percent: m.quota_percent,
                idea_rank: m.idea_rank,
                planning_rank: m.planning_rank,
                build_rank: m.build_rank,
                review_rank: m.review_rank,
                idea_weight: m.idea_weight,
                planning_weight: m.planning_weight,
                build_weight: m.build_weight,
                review_weight: m.review_weight,
            })
            .collect(),
        quota_errors: errors
            .iter()
            .map(|e| CachedQuotaError {
                vendor: vendor_str(e.vendor).to_string(),
                message: e.message.clone(),
            })
            .collect(),
    };

    let text = serde_json::to_string_pretty(&cached).context("failed to serialise cache")?;
    fs::write(&path, text).context("failed to write cache file")?;
    Ok(())
}

fn vendor_str(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude",
        VendorKind::Codex => "codex",
        VendorKind::Gemini => "gemini",
        VendorKind::Kimi => "kimi",
    }
}

fn parse_vendor(s: &str) -> Option<VendorKind> {
    match s {
        "claude" => Some(VendorKind::Claude),
        "codex" => Some(VendorKind::Codex),
        "gemini" => Some(VendorKind::Gemini),
        "kimi" => Some(VendorKind::Kimi),
        _ => None,
    }
}
