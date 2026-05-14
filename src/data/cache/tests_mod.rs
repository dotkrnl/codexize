use super::*;
use tempfile::TempDir;

fn sample_entries() -> Vec<DashboardEntry> {
    vec![DashboardEntry {
        name: "claude-sonnet".to_string(),
        ipbr_phase_scores: IpbrPhaseScores::default(),
        score_source: ScoreSource::None,
        display_order: 0,
    }]
}

fn sample_quotas() -> QuotaPayload {
    let mut inner = BTreeMap::new();
    inner.insert("claude-sonnet".to_string(), Some(75u8));
    let mut values = BTreeMap::new();
    values.insert("claude".to_string(), inner);
    QuotaPayload::from(values)
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
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    let loaded = load(dir.path());
    let dash = loaded.dashboard.unwrap();
    assert!(!dash.expired);
    assert_eq!(dash.data.len(), 1);
    assert_eq!(dash.data[0].name, "claude-sonnet");
}

#[test]
fn save_and_load_quotas() {
    let dir = TempDir::new().unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();
    let loaded = load(dir.path());
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
    save_quota_resets(dir.path(), &sample_resets()).unwrap();
    let loaded = load(dir.path());
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

    let loaded = load(dir.path());

    assert!(loaded.quotas.is_some());
    assert!(loaded.quota_resets.is_none());
}

#[test]
fn non_current_cache_version_is_ignored() {
    let dir = TempDir::new().unwrap();
    let payload = serde_json::json!({
        "version": CACHE_VERSION - 1,
        "dashboard": {
            "fetched_at": now_secs(),
            "data": [{
                "name": "claude-opus-4.7",
                "ipbr_phase_scores": {"idea": 91.0, "planning": 90.0, "build": 89.0, "review": 88.0},
                "score_source": "ipbr",
                "display_order": 0,
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
        serde_json::to_string(&payload).unwrap(),
    )
    .unwrap();

    let loaded = load(dir.path());

    assert!(loaded.dashboard.is_none());
    assert!(loaded.quotas.is_none());
}

#[test]
fn save_after_version_mismatch_rewrites_at_current_version() {
    let dir = TempDir::new().unwrap();
    let payload = serde_json::json!({
        "version": CACHE_VERSION - 1,
        "dashboard": {
            "fetched_at": now_secs(),
            "data": []
        },
        "quotas": {
            "fetched_at": now_secs(),
            "data": sample_quotas()
        }
    });
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(
        dir.path().join("models.json"),
        serde_json::to_string(&payload).unwrap(),
    )
    .unwrap();

    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();

    let loaded = load(dir.path());
    assert!(
        loaded.dashboard.is_some(),
        "current-version dashboard rewritten on save"
    );
    let quotas = loaded
        .quotas
        .expect("freshly-saved quota section must round-trip");
    assert_eq!(
        quotas
            .data
            .get("claude")
            .unwrap()
            .get("claude-sonnet")
            .unwrap(),
        &Some(75)
    );
}

/// A current-version dashboard entry that omits ipbr fields (e.g.
/// from a fresh save before ipbr scores are attached) loads with
/// per-phase scores defaulting to `None` and the provenance to a
/// non-`Ipbr` value, so missing data cannot masquerade as ipbr authority.
#[test]
fn entry_missing_ipbr_fields_defaults_to_unscored_non_ipbr() {
    let dir = TempDir::new().unwrap();
    let payload = serde_json::json!({
        "version": CACHE_VERSION,
        "dashboard": {
            "fetched_at": now_secs(),
            "data": [{
                "name": "claude-sonnet",
                "display_order": 0
            }]
        }
    });
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(
        dir.path().join("models.json"),
        serde_json::to_string(&payload).unwrap(),
    )
    .unwrap();

    let loaded = load(dir.path());
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
}

#[test]
fn sections_are_independent() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_some());
    assert!(loaded.quotas.is_none());

    save_quotas(dir.path(), &sample_quotas()).unwrap();
    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_some());
    assert!(loaded.quotas.is_some());
}

#[test]
fn version_mismatch_drops_dashboard_only() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();
    let path = dir.path().join("models.json");
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    file.version = 999;
    fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

    let loaded = load(dir.path());
    assert!(
        loaded.dashboard.is_none(),
        "dashboard payload is dropped on any version mismatch"
    );
    assert!(
        loaded.quotas.is_none(),
        "quota payload is dropped on version mismatch (v7 reshape)"
    );
}

#[test]
fn older_cache_file_drops_all_versioned_sections() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();
    let path = dir.path().join("models.json");
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    file.version = 3;
    fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_none());
    assert!(
        loaded.quotas.is_none(),
        "old quota payload is dropped because the v7 quota schema differs"
    );
}

#[test]
fn ttl_expiry_dashboard() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    let path = dir.path().join("models.json");
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    if let Some(ref mut s) = file.dashboard {
        s.fetched_at = now_secs() - DASHBOARD_TTL.as_secs() - 1;
    }
    fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

    let loaded = load(dir.path());
    assert!(loaded.dashboard.unwrap().expired);
}

#[test]
fn ttl_expiry_quotas() {
    let dir = TempDir::new().unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();
    let path = dir.path().join("models.json");
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    if let Some(ref mut s) = file.quotas {
        s.fetched_at = now_secs() - QUOTA_TTL.as_secs() - 1;
    }
    fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

    let loaded = load(dir.path());
    assert!(loaded.quotas.unwrap().expired);
}

#[test]
fn write_cache_file_produces_valid_json() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    let text = fs::read_to_string(dir.path().join("models.json")).unwrap();
    let file: CacheFile = serde_json::from_str(&text).unwrap();
    assert_eq!(file.version, CACHE_VERSION);
}

#[test]
fn cache_file_writes_current_dashboard_entry_shape() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();

    let text = fs::read_to_string(dir.path().join("models.json")).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    let entry = &parsed["dashboard"]["data"][0];
    let keys = entry
        .as_object()
        .expect("dashboard entry should be an object");
    assert_eq!(
        keys.keys().cloned().collect::<Vec<_>>(),
        vec!["display_order", "ipbr_phase_scores", "name", "score_source"]
    );
}

#[test]
fn missing_cache_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_none());
    assert!(loaded.quotas.is_none());
}

#[test]
fn corrupt_cache_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("models.json"), "not json at all").unwrap();
    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_none());
    assert!(loaded.quotas.is_none());
}

#[test]
fn save_dashboard_preserves_existing_quotas() {
    let dir = TempDir::new().unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();

    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_some());
    assert!(loaded.quotas.is_some());
}

#[test]
fn save_quotas_preserves_existing_dashboard() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();

    let loaded = load(dir.path());
    assert!(loaded.dashboard.is_some());
    assert!(loaded.quotas.is_some());
}

#[test]
fn explicit_dir_round_trip_writes_under_supplied_path() {
    // The public API takes an explicit cache directory; the operator's
    // configured `paths.cache_root` flows in from the App layer, and
    // every entry point must persist under exactly the directory it was
    // handed.
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();

    let path = dir.path().join("models.json");
    assert!(path.exists(), "expected cache file at {path:?}");

    let loaded = load(dir.path());
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
}

#[test]
fn load_returns_empty_when_supplied_dir_is_missing() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("does-not-exist");
    let loaded = load(&missing);
    assert!(loaded.dashboard.is_none());
    assert!(loaded.quotas.is_none());
}
