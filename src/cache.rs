use crate::cache_lock;
use crate::selection::{ModelStatus, QuotaError, VendorKind};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TTL: Duration = Duration::from_secs(30 * 60);

pub const CACHE_VERSION: u32 = 2;
pub const DASHBOARD_TTL: Duration = Duration::from_secs(30 * 60);
pub const QUOTA_TTL: Duration = Duration::from_secs(10 * 60);

// ---------------------------------------------------------------------------
// Schema v2 types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CacheFile {
    pub version: u32,
    pub dashboard: Option<Section<Vec<DashboardEntry>>>,
    pub quotas: Option<Section<QuotaPayload>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Section<T> {
    pub fetched_at: u64,
    pub data: T,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DashboardEntry {
    pub vendor: String,
    pub name: String,
    pub overall_score: f64,
    pub current_score: f64,
    pub standard_error: f64,
    pub axes: Vec<(String, f64)>,
    pub display_order: usize,
    #[serde(default)]
    pub fallback_from: Option<String>,
}

/// Per-vendor map of model name → optional quota percentage.
pub type QuotaPayload = BTreeMap<String, BTreeMap<String, Option<u8>>>;

pub struct LoadedCache {
    pub dashboard: Option<LoadedSection<Vec<DashboardEntry>>>,
    pub quotas: Option<LoadedSection<QuotaPayload>>,
}

pub struct LoadedSection<T> {
    pub data: T,
    pub expired: bool,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn default_cache_dir() -> PathBuf {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".codexize").join("cache")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Public API — schema v2
// ---------------------------------------------------------------------------

pub fn load() -> LoadedCache {
    load_at(&default_cache_dir())
}

pub fn save_dashboard(entries: &[DashboardEntry]) -> Result<()> {
    save_dashboard_at(&default_cache_dir(), entries)
}

pub fn save_quotas(payload: &QuotaPayload) -> Result<()> {
    save_quotas_at(&default_cache_dir(), payload)
}

// ---------------------------------------------------------------------------
// Path-parameterized implementations
// ---------------------------------------------------------------------------

fn load_at(dir: &Path) -> LoadedCache {
    let empty = LoadedCache {
        dashboard: None,
        quotas: None,
    };
    let text = match fs::read_to_string(dir.join("models.json")) {
        Ok(t) => t,
        Err(_) => return empty,
    };
    let file: CacheFile = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(_) => return empty,
    };
    if file.version != CACHE_VERSION {
        return empty;
    }
    let now = now_secs();
    LoadedCache {
        dashboard: file.dashboard.map(|s| LoadedSection {
            expired: now.saturating_sub(s.fetched_at) >= DASHBOARD_TTL.as_secs(),
            data: s.data,
        }),
        quotas: file.quotas.map(|s| LoadedSection {
            expired: now.saturating_sub(s.fetched_at) >= QUOTA_TTL.as_secs(),
            data: s.data,
        }),
    }
}

fn save_dashboard_at(dir: &Path, entries: &[DashboardEntry]) -> Result<()> {
    let lock = dir.join("models.json.lock");
    cache_lock::with_lock(&lock, || {
        let mut file = load_raw_or_default(dir);
        file.dashboard = Some(Section {
            fetched_at: now_secs(),
            data: entries.to_vec(),
        });
        atomic_write(dir, &file)
    })
}

fn save_quotas_at(dir: &Path, payload: &QuotaPayload) -> Result<()> {
    let lock = dir.join("models.json.lock");
    cache_lock::with_lock(&lock, || {
        let mut file = load_raw_or_default(dir);
        file.quotas = Some(Section {
            fetched_at: now_secs(),
            data: payload.clone(),
        });
        atomic_write(dir, &file)
    })
}

// ---------------------------------------------------------------------------
// Legacy adapters (temporary — removed once app migrates)
// ---------------------------------------------------------------------------

pub fn load_legacy_model_statuses() -> Option<(Vec<ModelStatus>, Vec<QuotaError>, bool)> {
    loaded_cache_to_legacy(load())
}

pub fn save_legacy_model_statuses(models: &[ModelStatus], errors: &[QuotaError]) -> Result<()> {
    let entries: Vec<DashboardEntry> = models
        .iter()
        .enumerate()
        .map(|(i, m)| DashboardEntry {
            vendor: vendor_str(m.vendor).to_string(),
            name: m.name.clone(),
            overall_score: 0.0,
            current_score: m.stupid_level.unwrap_or(0) as f64,
            standard_error: 0.0,
            axes: Vec::new(),
            display_order: i,
            fallback_from: m.fallback_from.clone(),
        })
        .collect();
    let mut quotas: QuotaPayload = BTreeMap::new();
    for model in models {
        quotas
            .entry(vendor_str(model.vendor).to_string())
            .or_default()
            .insert(model.name.clone(), model.quota_percent);
    }
    for error in errors {
        // REVIEWER: Legacy wrapper ambiguity: schema v2 has no explicit error
        // section, so we persist a vendor-level failure marker as an empty
        // quota map and rehydrate it back into `QuotaError` on load.
        quotas
            .entry(vendor_str(error.vendor).to_string())
            .or_default();
    }

    let dir = default_cache_dir();
    let lock = dir.join("models.json.lock");
    cache_lock::with_lock(&lock, || {
        let mut file = load_raw_or_default(&dir);
        let fetched_at = now_secs();
        file.dashboard = Some(Section {
            fetched_at,
            data: entries,
        });
        file.quotas = Some(Section {
            fetched_at,
            data: quotas,
        });
        atomic_write(&dir, &file)
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn load_raw_or_default(dir: &Path) -> CacheFile {
    let text = match fs::read_to_string(dir.join("models.json")) {
        Ok(t) => t,
        Err(_) => {
            return CacheFile {
                version: CACHE_VERSION,
                dashboard: None,
                quotas: None,
            }
        }
    };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(f) if f.version == CACHE_VERSION => f,
        _ => CacheFile {
            version: CACHE_VERSION,
            dashboard: None,
            quotas: None,
        },
    }
}

fn loaded_cache_to_legacy(loaded: LoadedCache) -> Option<(Vec<ModelStatus>, Vec<QuotaError>, bool)> {
    let dashboard = loaded.dashboard?;
    let quotas = loaded.quotas;
    let expired = dashboard.expired || quotas.as_ref().is_some_and(|section| section.expired);

    let mut quota_errors = Vec::new();
    let mut parsed_quotas: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
    if let Some(section) = quotas {
        for (vendor_name, model_quotas) in section.data {
            let Some(vendor) = parse_vendor(&vendor_name) else {
                continue;
            };
            if model_quotas.is_empty() {
                quota_errors.push(QuotaError {
                    vendor,
                    message: "cached quota refresh previously failed".to_string(),
                });
            }
            parsed_quotas.insert(vendor, model_quotas);
        }
    }

    let models: Vec<ModelStatus> = dashboard
        .data
        .into_iter()
        .filter_map(|e| {
            let vendor = parse_vendor(&e.vendor)?;
            let quota_percent = parsed_quotas
                .get(&vendor)
                .and_then(|by_model| by_model.get(&e.name))
                .copied()
                .flatten();
            Some(ModelStatus {
                vendor,
                name: e.name,
                stupid_level: Some(e.current_score.round().clamp(0.0, 99.0) as u8),
                quota_percent,
                idea_rank: 99,
                planning_rank: 99,
                build_rank: 99,
                review_rank: 99,
                idea_weight: 0.0,
                planning_weight: 0.0,
                build_weight: 0.0,
                review_weight: 0.0,
                fallback_from: e.fallback_from,
            })
        })
        .collect();

    if models.is_empty() {
        return None;
    }
    Some((models, quota_errors, expired))
}

fn atomic_write(dir: &Path, file: &CacheFile) -> Result<()> {
    fs::create_dir_all(dir).context("failed to create cache directory")?;

    let tmp_path = dir.join(".models.json.tmp");
    let final_path = dir.join("models.json");
    let text = serde_json::to_string_pretty(file).context("failed to serialise cache")?;
    {
        let mut tmp = fs::File::create(&tmp_path).context("failed to create temp cache file")?;
        tmp.write_all(text.as_bytes())
            .context("failed to write temp cache file")?;
        tmp.sync_all().context("failed to sync temp cache file")?;
    }
    fs::rename(&tmp_path, &final_path).context("failed to rename temp cache file")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_entries() -> Vec<DashboardEntry> {
        vec![DashboardEntry {
            vendor: "claude".to_string(),
            name: "claude-sonnet".to_string(),
            overall_score: 85.0,
            current_score: 82.0,
            standard_error: 1.5,
            axes: vec![("coding".to_string(), 90.0)],
            display_order: 0,
            fallback_from: None,
        }]
    }

    fn sample_quotas() -> QuotaPayload {
        let mut inner = BTreeMap::new();
        inner.insert("claude-sonnet".to_string(), Some(75u8));
        let mut payload = BTreeMap::new();
        payload.insert("claude".to_string(), inner);
        payload
    }

    #[test]
    fn save_and_load_dashboard() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        let loaded = load_at(dir.path());
        let dash = loaded.dashboard.unwrap();
        assert!(!dash.expired);
        assert_eq!(dash.data.len(), 1);
        assert_eq!(dash.data[0].name, "claude-sonnet");
        assert_eq!(dash.data[0].overall_score, 85.0);
    }

    #[test]
    fn save_and_load_quotas() {
        let dir = TempDir::new().unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();
        let loaded = load_at(dir.path());
        let q = loaded.quotas.unwrap();
        assert!(!q.expired);
        assert_eq!(
            q.data.get("claude").unwrap().get("claude-sonnet").unwrap(),
            &Some(75)
        );
    }

    #[test]
    fn sections_are_independent() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_some());
        assert!(loaded.quotas.is_none());

        save_quotas_at(dir.path(), &sample_quotas()).unwrap();
        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_some());
        assert!(loaded.quotas.is_some());
    }

    #[test]
    fn version_mismatch_returns_none() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        let path = dir.path().join("models.json");
        let mut file: CacheFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        file.version = 999;
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_none());
        assert!(loaded.quotas.is_none());
    }

    #[test]
    fn ttl_expiry_dashboard() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        let path = dir.path().join("models.json");
        let mut file: CacheFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        if let Some(ref mut s) = file.dashboard {
            s.fetched_at = now_secs() - DASHBOARD_TTL.as_secs() - 1;
        }
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.unwrap().expired);
    }

    #[test]
    fn ttl_expiry_quotas() {
        let dir = TempDir::new().unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();
        let path = dir.path().join("models.json");
        let mut file: CacheFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        if let Some(ref mut s) = file.quotas {
            s.fetched_at = now_secs() - QUOTA_TTL.as_secs() - 1;
        }
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let loaded = load_at(dir.path());
        assert!(loaded.quotas.unwrap().expired);
    }

    #[test]
    fn atomic_write_produces_valid_json() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        let text = fs::read_to_string(dir.path().join("models.json")).unwrap();
        let file: CacheFile = serde_json::from_str(&text).unwrap();
        assert_eq!(file.version, CACHE_VERSION);
    }

    #[test]
    fn missing_cache_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_none());
        assert!(loaded.quotas.is_none());
    }

    #[test]
    fn corrupt_cache_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("models.json"), "not json at all").unwrap();
        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_none());
        assert!(loaded.quotas.is_none());
    }

    #[test]
    fn save_dashboard_preserves_existing_quotas() {
        let dir = TempDir::new().unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();

        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_some());
        assert!(loaded.quotas.is_some());
    }

    #[test]
    fn save_quotas_preserves_existing_dashboard() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();

        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_some());
        assert!(loaded.quotas.is_some());
    }

    #[test]
    fn legacy_adapter_maps_quota_percent_from_quota_section() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();

        let loaded = load_at(dir.path());
        let (models, errors, expired) =
            loaded_cache_to_legacy(loaded).expect("legacy adapter should return models");

        assert!(!expired);
        assert!(errors.is_empty());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].quota_percent, Some(75));
    }

    #[test]
    fn legacy_adapter_surfaces_cached_quota_failure_markers() {
        let loaded = LoadedCache {
            dashboard: Some(LoadedSection {
                expired: false,
                data: sample_entries(),
            }),
            quotas: Some(LoadedSection {
                expired: false,
                data: BTreeMap::from([("claude".to_string(), BTreeMap::new())]),
            }),
        };

        let (_, errors, _) = loaded_cache_to_legacy(loaded).expect("legacy adapter should return models");

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].vendor, VendorKind::Claude);
    }
}
