use crate::data::cache_lock;
use crate::selection::{IpbrPhaseScores, ScoreSource, SubscriptionKind};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub const TTL: Duration = Duration::from_secs(30 * 60);
/// Bump from v4 → v5 because model-first assembly replaces the
/// per-vendor `CachedModel` shape with rows carrying a `candidates`
/// vector and `selected_candidate` index. v4 entries lack these
/// fields; treating them as v5 would lose candidate data on load.
///
/// Versioning applies to the dashboard section only. Quota and quota-
/// reset sections are loaded independently under their own TTL, because
/// their schema is unchanged across this bump and the task requires
/// provider quota cache behavior to stay intact.
pub const CACHE_VERSION: u32 = 5;
pub const DASHBOARD_TTL: Duration = Duration::from_secs(30 * 60);
pub const QUOTA_TTL: Duration = Duration::from_secs(10 * 60);
// ---------------------------------------------------------------------------
// Schema v4 types
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
    quotas: Option<Section<QuotaPayload>>,
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
    pub vendor: String,
    pub name: String,
    /// Cosmetic display-only summary score. MUST NOT drive phase ranking,
    /// auto-selection eligibility, or vendor backfill ordering.
    pub overall_score: f64,
    /// Cosmetic display-only summary score. Same constraint as
    /// `overall_score`.
    pub current_score: f64,
    pub standard_error: f64,
    /// Values are 0.0..=1.0 floats from the aistupidlevel API; keys are
    /// lowercased camelCase. Backfill semantics are owned by the selection layer.
    pub axes: Vec<(String, f64)>,
    #[serde(default)]
    pub axis_provenance: BTreeMap<String, String>,
    /// Per-phase ipbr rank scores. `#[serde(default)]` so a v4 entry
    /// written before ipbr ingestion lands deserializes with all phases
    /// `None`, preserving the unscored-vs-known distinction.
    #[serde(default)]
    pub ipbr_phase_scores: IpbrPhaseScores,
    /// Provenance marker for the per-phase scores. Defaults to
    /// `ScoreSource::None` so a missing field cannot be interpreted as
    /// `Ipbr` authority.
    #[serde(default)]
    pub score_source: ScoreSource,
    /// `true` when this row matched an ipbr scoreboard row by normalized
    /// exact key. Defaults to `false` so legacy/inventory-only entries
    /// do not appear matched.
    #[serde(default)]
    pub ipbr_row_matched: bool,
    #[serde(default)]
    pub ipbr_match_key: Option<String>,
    #[serde(default)]
    pub route_underlying_vendor: Option<SubscriptionKind>,
    /// Opencode sub-provider (`opencode` or `opencode-go`). Persisted so a
    /// cached entry survives a restart and the launch boundary can still
    /// pick the right tier qualifier without re-querying the CLI.
    #[serde(default)]
    pub route_provider: Option<String>,
    pub display_order: usize,
    #[serde(default)]
    pub fallback_from: Option<String>,
}
/// Per-vendor map of model name → optional quota percentage.
pub type QuotaPayload = BTreeMap<String, BTreeMap<String, Option<u8>>>;
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
// Public API — schema v4
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
    // Dashboard payload is dropped on any version mismatch so old
    // aistupidlevel-shaped entries cannot be read as ipbr phase
    // authority. Quota / quota-reset sections fall through unchanged.
    let dashboard_section = if parsed.version == CACHE_VERSION {
        parsed
            .dashboard
            .and_then(|raw| serde_json::from_value::<Section<Vec<DashboardEntry>>>(raw).ok())
    } else {
        None
    };
    let now = now_secs();
    LoadedCache {
        dashboard: dashboard_section.map(|s| LoadedSection {
            expired: now.saturating_sub(s.fetched_at) >= DASHBOARD_TTL.as_secs(),
            data: s.data,
        }),
        quotas: parsed.quotas.map(|s| LoadedSection {
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
pub fn save_quotas(dir: &Path, payload: &QuotaPayload) -> Result<()> {
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
pub fn save_quota_resets(dir: &Path, payload: &ResetPayload) -> Result<()> {
    let lock = dir.join("models.json.lock");
    cache_lock::with_lock(&lock, || {
        let mut file = load_raw_or_default(dir);
        file.quota_resets = Some(Section {
            fetched_at: now_secs(),
            data: payload.clone(),
        });
        atomic_write(dir, &file)
    })
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
    // Same per-section policy as `load_at`: drop the dashboard payload on
    // version mismatch but preserve quota / quota-reset sections so a
    // dashboard schema bump never invalidates valid quota cache data.
    let dashboard = if parsed.version == CACHE_VERSION {
        parsed
            .dashboard
            .and_then(|raw| serde_json::from_value::<Section<Vec<DashboardEntry>>>(raw).ok())
    } else {
        None
    };
    CacheFile {
        version: CACHE_VERSION,
        dashboard,
        quotas: parsed.quotas,
        quota_resets: parsed.quota_resets,
    }
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
#[cfg(test)]
mod tests_mod;
