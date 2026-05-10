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

/// An older cache file (which stores legacy aistupidlevel-shaped fields)
/// MUST NOT be readable as a current dashboard. Version bumps invalidate
/// stale dashboard payloads so old score/name semantics cannot survive;
/// the v7 quota-payload reshape kept the quota section dropped on version
/// mismatch as well.
#[test]
fn old_v7_cache_cannot_masquerade_as_current_cache() {
    let dir = TempDir::new().unwrap();
    let old_payload = serde_json::json!({
        "version": 7,
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
        serde_json::to_string(&old_payload).unwrap(),
    )
    .unwrap();

    let loaded = load(dir.path());

    assert!(
        loaded.dashboard.is_none(),
        "old dashboard must not be readable under v{CACHE_VERSION}"
    );
    assert!(
        loaded.quotas.is_none(),
        "v7 quota payload must also be dropped on a version mismatch"
    );
}

#[test]
fn old_v8_dashboard_payload_is_invalidated() {
    let dir = TempDir::new().unwrap();
    let old_payload = serde_json::json!({
        "version": 8,
        "dashboard": {
            "fetched_at": now_secs(),
            "data": [{
                "name": "claude-opus-4.6",
                "ipbr_phase_scores": {"idea": 91.0, "planning": 90.0, "build": 89.0, "review": 88.0},
                "score_source": "ipbr",
                "display_order": 0
            }]
        },
        "quotas": null
    });
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(
        dir.path().join("models.json"),
        serde_json::to_string(&old_payload).unwrap(),
    )
    .unwrap();

    let loaded = load(dir.path());

    assert!(
        loaded.dashboard.is_none(),
        "v8 dashboard payload must be refreshed under v{CACHE_VERSION}"
    );
}

/// On a version-mismatch reload the entire cache is rewritten at the
/// current version, and a subsequent save round-trips correctly
/// without leaking the old payload shapes back in.
#[test]
fn save_after_version_mismatch_rewrites_at_current_version() {
    let dir = TempDir::new().unwrap();
    let old_payload = serde_json::json!({
        "version": 7,
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
        serde_json::to_string(&old_payload).unwrap(),
    )
    .unwrap();

    // Saving a fresh dashboard rewrites the file at the current
    // version. The pre-bump quota payload is dropped (different shape),
    // so the post-save quota section must come from a fresh save —
    // exercise that round-trip explicitly here.
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
                "vendor": "claude",
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
fn atomic_write_produces_valid_json() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    let text = fs::read_to_string(dir.path().join("models.json")).unwrap();
    let file: CacheFile = serde_json::from_str(&text).unwrap();
    assert_eq!(file.version, CACHE_VERSION);
}

#[test]
fn cache_file_omits_legacy_rank_weight_and_aistupidlevel_fields() {
    let dir = TempDir::new().unwrap();
    save_dashboard(dir.path(), &sample_entries()).unwrap();
    save_quotas(dir.path(), &sample_quotas()).unwrap();

    let text = fs::read_to_string(dir.path().join("models.json")).unwrap();

    let forbidden = vec![
        "idea_rank".to_string(),
        "planning_rank".to_string(),
        "build_rank".to_string(),
        "review_rank".to_string(),
        "idea_weight".to_string(),
        "planning_weight".to_string(),
        "build_weight".to_string(),
        "review_weight".to_string(),
        "stupid_level".to_string(),
        // v8 dropped these legacy aistupidlevel-shaped fields.
        "axes".to_string(),
        "axis_provenance".to_string(),
        "overall_score".to_string(),
        "current_score".to_string(),
        "standard_error".to_string(),
        "fallback_from".to_string(),
        // v10 dropped raw upstream vendor strings and dashboard/IPBR
        // match metadata.
        "vendor".to_string(),
        format!("{}{}", "ipbr_", "row_matched"),
        format!("{}{}", "ipbr_", "match_key"),
    ];
    for forbidden in forbidden {
        assert!(
            !text.contains(&forbidden),
            "cache file unexpectedly contained legacy field {forbidden}"
        );
    }
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
