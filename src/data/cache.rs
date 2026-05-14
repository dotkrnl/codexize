use crate::data::cache_lock;
use crate::selection::{IpbrPhaseScores, ScoreSource, SubscriptionKind};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub const TTL: Duration = Duration::from_secs(10 * 60);
/// Current cache schema version. Cached dashboard rows must refresh into the
/// exact provider/IPBR canonical shape.
pub const CACHE_VERSION: u32 = 10;
pub const DASHBOARD_TTL: Duration = Duration::from_secs(10 * 60);
pub const QUOTA_TTL: Duration = Duration::from_secs(10 * 60);
// ---------------------------------------------------------------------------
// Cache file types
// ---------------------------------------------------------------------------
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CacheFile {
    pub version: u32,
    pub dashboard: Option<Section<Vec<DashboardEntry>>>,
    pub quotas: Option<Section<QuotaPayload>>,
    #[serde(default)]
    pub quota_resets: Option<Section<ResetPayload>>,
}
/// Lenient parse used during load. The dashboard payload is held as raw
/// JSON so we can decide whether to deserialize it based on the file's
/// `version`, while quota / quota-reset sections — whose schema is stable
/// across this version bump — deserialize directly and survive a
/// dashboard-only schema change.
#[derive(Deserialize, Debug)]
struct VersionedFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    dashboard: Option<serde_json::Value>,
    #[serde(default)]
    quotas: Option<serde_json::Value>,
    #[serde(default)]
    quota_resets: Option<Section<ResetPayload>>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Section<T> {
    pub fetched_at: u64,
    pub data: T,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DashboardEntry {
    pub name: String,
    /// Per-phase ipbr rank scores. `None` per phase means the matched
    /// ipbr row did not provide that phase score.
    #[serde(default)]
    pub ipbr_phase_scores: IpbrPhaseScores,
    /// Provenance marker for the per-phase scores. Defaults to
    /// `ScoreSource::None` so a missing field cannot be interpreted as
    /// `Ipbr` authority.
    #[serde(default)]
    pub score_source: ScoreSource,
    pub display_order: usize,
}
/// Per-vendor map of model name → optional quota percentage, paired
/// with the set of subscriptions whose most recent quota fetch failed.
/// Selection treats a failed-subscription provider's effective quota as
/// 50% (per spec §quota-failure plumbing). The struct deref's to its
/// values map so the existing per-subscription/per-model lookup pattern
/// keeps working without touching every call site.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaPayload {
    #[serde(default)]
    pub values: BTreeMap<String, BTreeMap<String, Option<u8>>>,
    #[serde(default)]
    pub failed_subscriptions: BTreeSet<SubscriptionKind>,
}

impl Deref for QuotaPayload {
    type Target = BTreeMap<String, BTreeMap<String, Option<u8>>>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl DerefMut for QuotaPayload {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}

impl From<BTreeMap<String, BTreeMap<String, Option<u8>>>> for QuotaPayload {
    fn from(values: BTreeMap<String, BTreeMap<String, Option<u8>>>) -> Self {
        Self {
            values,
            failed_subscriptions: BTreeSet::new(),
        }
    }
}
/// Per-vendor map of model name → optional quota reset timestamp.
pub type ResetPayload = BTreeMap<String, BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>>;
pub struct LoadedCache {
    pub dashboard: Option<LoadedSection<Vec<DashboardEntry>>>,
    pub quotas: Option<LoadedSection<QuotaPayload>>,
    pub quota_resets: Option<LoadedSection<ResetPayload>>,
}
pub struct LoadedSection<T> {
    pub data: T,
    pub expired: bool,
}
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
// ---------------------------------------------------------------------------
// Public API
//
// Every entry point takes an explicit `dir`. Callers thread the cache
// directory from `paths.cache_root` (loaded from `~/.codexize/config.toml`)
// so an operator override is honored.
// ---------------------------------------------------------------------------
pub fn load(dir: &Path) -> LoadedCache {
    let empty = LoadedCache {
        dashboard: None,
        quotas: None,
        quota_resets: None,
    };
    let text = match fs::read_to_string(dir.join("models.json")) {
        Ok(t) => t,
        Err(_) => return empty,
    };
    let parsed: VersionedFile = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(_) => return empty,
    };
    // Dashboard and quota payloads are only trusted when the cache file is the
    // current schema version. Quota-reset shape is stable.
    let dashboard_section = if parsed.version == CACHE_VERSION {
        parsed
            .dashboard
            .and_then(|raw| serde_json::from_value::<Section<Vec<DashboardEntry>>>(raw).ok())
    } else {
        None
    };
    let quota_section = if parsed.version == CACHE_VERSION {
        parsed
            .quotas
            .and_then(|raw| serde_json::from_value::<Section<QuotaPayload>>(raw).ok())
    } else {
        None
    };
    let now = now_secs();
    LoadedCache {
        dashboard: dashboard_section.map(|s| LoadedSection {
            expired: now.saturating_sub(s.fetched_at) >= DASHBOARD_TTL.as_secs(),
            data: s.data,
        }),
        quotas: quota_section.map(|s| LoadedSection {
            expired: now.saturating_sub(s.fetched_at) >= QUOTA_TTL.as_secs(),
            data: s.data,
        }),
        quota_resets: parsed.quota_resets.map(|s| LoadedSection {
            expired: now.saturating_sub(s.fetched_at) >= QUOTA_TTL.as_secs(),
            data: s.data,
        }),
    }
}
pub fn save_dashboard(dir: &Path, entries: &[DashboardEntry]) -> Result<()> {
    cache_lock::with_lock(&lock_path(dir), || save_dashboard_unlocked(dir, entries))
}
pub fn save_quotas(dir: &Path, payload: &QuotaPayload) -> Result<()> {
    cache_lock::with_lock(&lock_path(dir), || save_quotas_unlocked(dir, payload))
}
pub fn save_quota_resets(dir: &Path, payload: &ResetPayload) -> Result<()> {
    cache_lock::with_lock(&lock_path(dir), || save_quota_resets_unlocked(dir, payload))
}

/// Path to the on-disk lock that serializes writers to `models.json`.
pub fn lock_path(dir: &Path) -> std::path::PathBuf {
    dir.join("models.json.lock")
}

/// Variants used by the publisher path, which has already acquired the lock
/// via `cache_lock::try_acquire` and would deadlock if the save routines
/// reacquired it. Callers MUST hold the cache lock for `dir`.
pub fn save_dashboard_unlocked(dir: &Path, entries: &[DashboardEntry]) -> Result<()> {
    let mut file = load_raw_or_default(dir);
    file.dashboard = Some(Section {
        fetched_at: now_secs(),
        data: entries.to_vec(),
    });
    write_cache_file(dir, &file)
}
pub fn save_quotas_unlocked(dir: &Path, payload: &QuotaPayload) -> Result<()> {
    let mut file = load_raw_or_default(dir);
    file.quotas = Some(Section {
        fetched_at: now_secs(),
        data: payload.clone(),
    });
    write_cache_file(dir, &file)
}
pub fn save_quota_resets_unlocked(dir: &Path, payload: &ResetPayload) -> Result<()> {
    let mut file = load_raw_or_default(dir);
    file.quota_resets = Some(Section {
        fetched_at: now_secs(),
        data: payload.clone(),
    });
    write_cache_file(dir, &file)
}
// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------
fn load_raw_or_default(dir: &Path) -> CacheFile {
    let empty = CacheFile {
        version: CACHE_VERSION,
        dashboard: None,
        quotas: None,
        quota_resets: None,
    };
    let text = match fs::read_to_string(dir.join("models.json")) {
        Ok(t) => t,
        Err(_) => return empty,
    };
    let parsed: VersionedFile = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(_) => return empty,
    };
    // Same per-section policy as `load`: only current-version dashboard and
    // quota payloads are trusted. Quota-reset shape is stable.
    let dashboard = if parsed.version == CACHE_VERSION {
        parsed
            .dashboard
            .and_then(|raw| serde_json::from_value::<Section<Vec<DashboardEntry>>>(raw).ok())
    } else {
        None
    };
    let quotas = if parsed.version == CACHE_VERSION {
        parsed
            .quotas
            .and_then(|raw| serde_json::from_value::<Section<QuotaPayload>>(raw).ok())
    } else {
        None
    };
    CacheFile {
        version: CACHE_VERSION,
        dashboard,
        quotas,
        quota_resets: parsed.quota_resets,
    }
}
fn write_cache_file(dir: &Path, file: &CacheFile) -> Result<()> {
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
pub mod watcher;
pub use watcher::{CACHE_POLL_INTERVAL, CacheWatcher, CacheWatcherOutcome};

#[cfg(test)]
mod tests_mod;
