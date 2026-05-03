use crate::cache_lock;
use crate::selection::{IpbrPhaseScores, ScoreSource};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TTL: Duration = Duration::from_secs(30 * 60);

/// Bump from v3 → v4 because dashboard entries now carry ipbr-specific
/// phase-score fields and a row-match flag. v3 entries cached old
/// aistupidlevel scores in axes/`overall_score`; treating those as v4
/// would let stale aistupidlevel data masquerade as ipbr authority on
/// load, which the spec explicitly forbids. Quota cache shape and TTL
/// are unchanged; quotas will simply be re-fetched on first run after
/// the bump.
pub const CACHE_VERSION: u32 = 4;
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
// Public API — schema v3
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

pub fn save_quota_resets(payload: &ResetPayload) -> Result<()> {
    save_quota_resets_at(&default_cache_dir(), payload)
}

// ---------------------------------------------------------------------------
// Path-parameterized implementations
// ---------------------------------------------------------------------------

fn load_at(dir: &Path) -> LoadedCache {
    let empty = LoadedCache {
        dashboard: None,
        quotas: None,
        quota_resets: None,
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
        quota_resets: file.quota_resets.map(|s| LoadedSection {
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

fn save_quota_resets_at(dir: &Path, payload: &ResetPayload) -> Result<()> {
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
    let text = match fs::read_to_string(dir.join("models.json")) {
        Ok(t) => t,
        Err(_) => {
            return CacheFile {
                version: CACHE_VERSION,
                dashboard: None,
                quotas: None,
                quota_resets: None,
            };
        }
    };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(f) if f.version == CACHE_VERSION => f,
        _ => CacheFile {
            version: CACHE_VERSION,
            dashboard: None,
            quotas: None,
            quota_resets: None,
        },
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_entries() -> Vec<DashboardEntry> {
        sample_entries_with_provenance(BTreeMap::new())
    }

    fn sample_entries_with_provenance(
        axis_provenance: BTreeMap<String, String>,
    ) -> Vec<DashboardEntry> {
        vec![DashboardEntry {
            vendor: "claude".to_string(),
            name: "claude-sonnet".to_string(),
            overall_score: 85.0,
            current_score: 82.0,
            standard_error: 1.5,
            axes: vec![("coding".to_string(), 90.0)],
            axis_provenance,
            ipbr_phase_scores: IpbrPhaseScores::default(),
            score_source: ScoreSource::None,
            ipbr_row_matched: false,
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

    fn sample_resets() -> ResetPayload {
        let mut inner = BTreeMap::new();
        inner.insert(
            "claude-sonnet".to_string(),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-04-30T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        );
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
    fn save_and_load_quota_resets() {
        let dir = TempDir::new().unwrap();
        save_quota_resets_at(dir.path(), &sample_resets()).unwrap();
        let loaded = load_at(dir.path());
        let resets = loaded.quota_resets.unwrap();
        assert!(!resets.expired);
        assert_eq!(
            resets
                .data
                .get("claude")
                .unwrap()
                .get("claude-sonnet")
                .unwrap(),
            sample_resets()
                .get("claude")
                .unwrap()
                .get("claude-sonnet")
                .unwrap()
        );
    }

    #[test]
    fn current_version_cache_without_quota_resets_loads() {
        let dir = TempDir::new().unwrap();
        let file = serde_json::json!({
            "version": CACHE_VERSION,
            "dashboard": null,
            "quotas": {
                "fetched_at": now_secs(),
                "data": sample_quotas()
            }
        });
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(
            dir.path().join("models.json"),
            serde_json::to_string(&file).unwrap(),
        )
        .unwrap();

        let loaded = load_at(dir.path());

        assert!(loaded.quotas.is_some());
        assert!(loaded.quota_resets.is_none());
    }

    /// A v3 cache file (which only stored aistupidlevel-shaped score data
    /// in `axes` / `overall_score` and lacked any ipbr-specific fields)
    /// MUST NOT be readable as a v4 dashboard. Otherwise old aistupidlevel
    /// scores could masquerade as ipbr phase authority on load — the
    /// exact failure mode the task 1 schema bump is meant to prevent.
    #[test]
    fn old_v3_cache_cannot_masquerade_as_ipbr_phase_authority() {
        let dir = TempDir::new().unwrap();
        let v3_payload = serde_json::json!({
            "version": 3,
            "dashboard": {
                "fetched_at": now_secs(),
                "data": [{
                    "vendor": "claude",
                    "name": "claude-sonnet",
                    "overall_score": 85.0,
                    "current_score": 82.0,
                    "standard_error": 1.5,
                    "axes": [["coding", 90.0]],
                    "axis_provenance": {},
                    "display_order": 0,
                    "fallback_from": null
                }]
            },
            "quotas": {
                "fetched_at": now_secs(),
                "data": sample_quotas()
            }
        });
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(
            dir.path().join("models.json"),
            serde_json::to_string(&v3_payload).unwrap(),
        )
        .unwrap();

        let loaded = load_at(dir.path());

        assert!(
            loaded.dashboard.is_none(),
            "v3 dashboard must not be readable under v{CACHE_VERSION}"
        );
        assert!(
            loaded.quotas.is_none(),
            "v3 sections are dropped wholesale; quotas will be re-fetched"
        );
    }

    /// A v4 dashboard entry written before ipbr ingestion lands omits the
    /// new ipbr fields. Loading must default the per-phase scores to
    /// `None` and the provenance to a non-`Ipbr` value so cached
    /// aistupidlevel `axes` / `overall_score` cannot pretend to be ipbr
    /// phase authority.
    #[test]
    fn v4_entry_missing_ipbr_fields_defaults_to_unscored_non_ipbr() {
        let dir = TempDir::new().unwrap();
        let payload = serde_json::json!({
            "version": CACHE_VERSION,
            "dashboard": {
                "fetched_at": now_secs(),
                "data": [{
                    "vendor": "claude",
                    "name": "claude-sonnet",
                    "overall_score": 85.0,
                    "current_score": 82.0,
                    "standard_error": 1.5,
                    "axes": [["coding", 90.0]],
                    "axis_provenance": {},
                    "display_order": 0,
                    "fallback_from": null
                }]
            }
        });
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(
            dir.path().join("models.json"),
            serde_json::to_string(&payload).unwrap(),
        )
        .unwrap();

        let loaded = load_at(dir.path());
        let entry = loaded
            .dashboard
            .expect("dashboard section should load")
            .data
            .into_iter()
            .next()
            .expect("entry should round-trip");

        assert_eq!(entry.ipbr_phase_scores, IpbrPhaseScores::default());
        assert_eq!(entry.ipbr_phase_scores.idea, None);
        assert_eq!(entry.ipbr_phase_scores.planning, None);
        assert_eq!(entry.ipbr_phase_scores.build, None);
        assert_eq!(entry.ipbr_phase_scores.review, None);
        assert_ne!(
            entry.score_source,
            ScoreSource::Ipbr,
            "missing provenance must not default to Ipbr"
        );
        assert_eq!(entry.score_source, ScoreSource::None);
        assert!(
            !entry.ipbr_row_matched,
            "no ipbr row matched until task 2 runs the ipbr lookup"
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
    fn v2_cache_file_returns_none() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();
        let path = dir.path().join("models.json");
        let mut file: CacheFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        file.version = 2;
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let loaded = load_at(dir.path());
        assert!(loaded.dashboard.is_none());
        assert!(loaded.quotas.is_none());
    }

    #[test]
    fn v3_cache_preserves_axis_provenance() {
        let dir = TempDir::new().unwrap();
        let provenance = BTreeMap::from([
            ("correctness".to_string(), "suite:hourly".to_string()),
            ("debugging".to_string(), "suite:deep".to_string()),
            ("taskcompletion".to_string(), "suite:tooling".to_string()),
            (
                "contextwindow".to_string(),
                "dropped:contextwindow".to_string(),
            ),
            ("edgecases".to_string(), "fallback:overall".to_string()),
        ]);
        save_dashboard_at(
            dir.path(),
            &sample_entries_with_provenance(provenance.clone()),
        )
        .unwrap();

        let loaded = load_at(dir.path());
        let dashboard = loaded.dashboard.unwrap();
        assert_eq!(dashboard.data[0].axis_provenance, provenance);
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
    fn cache_file_omits_legacy_rank_and_weight_fields() {
        let dir = TempDir::new().unwrap();
        save_dashboard_at(dir.path(), &sample_entries()).unwrap();
        save_quotas_at(dir.path(), &sample_quotas()).unwrap();

        let text = fs::read_to_string(dir.path().join("models.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();

        for forbidden in [
            "idea_rank",
            "planning_rank",
            "build_rank",
            "review_rank",
            "idea_weight",
            "planning_weight",
            "build_weight",
            "review_weight",
            "stupid_level",
        ] {
            assert!(
                !text.contains(forbidden),
                "cache file unexpectedly contained legacy field {forbidden}"
            );
        }

        assert!(
            text.contains("axis_provenance"),
            "cache file should contain the axis_provenance field"
        );
        assert!(
            json.pointer("/dashboard/data/0/axis_provenance").is_some(),
            "axis_provenance is a present cache field, not a legacy omission"
        );
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

    fn with_home_override<R>(dir: &TempDir, f: impl FnOnce() -> R) -> R {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("HOME");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match original {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        outcome.unwrap()
    }

    #[test]
    fn public_load_save_wrappers_round_trip_via_home() {
        let dir = TempDir::new().unwrap();
        with_home_override(&dir, || {
            // Both public save wrappers must persist under
            // $HOME/.codexize/cache/models.json without any explicit path.
            save_dashboard(&sample_entries()).unwrap();
            save_quotas(&sample_quotas()).unwrap();

            let path = dir
                .path()
                .join(".codexize")
                .join("cache")
                .join("models.json");
            assert!(path.exists(), "expected default-dir cache file at {path:?}");

            // The public load wrapper reads through the same default dir
            // and surfaces both sections written above.
            let loaded = load();
            let dash = loaded.dashboard.expect("dashboard section round-trips");
            assert_eq!(dash.data[0].name, "claude-sonnet");
            let quotas = loaded.quotas.expect("quota section round-trips");
            assert_eq!(
                quotas
                    .data
                    .get("claude")
                    .unwrap()
                    .get("claude-sonnet")
                    .unwrap(),
                &Some(75)
            );
        });
    }

    #[test]
    fn public_load_returns_empty_when_default_dir_is_missing() {
        let dir = TempDir::new().unwrap();
        with_home_override(&dir, || {
            let loaded = load();
            assert!(loaded.dashboard.is_none());
            assert!(loaded.quotas.is_none());
        });
    }
}
