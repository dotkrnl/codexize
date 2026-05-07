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
        ipbr_match_key: None,
        route_underlying_vendor: None,
        route_provider: None,
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
///
/// Versioning applies to the dashboard section only: an unrelated
/// quota section in the same file must still load under its normal
/// TTL, so a dashboard schema bump never forces a quota re-fetch.
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
    let quotas = loaded
        .quotas
        .expect("quota section is independent of dashboard schema version");
    assert!(
        !quotas.expired,
        "quota TTL is unaffected by a dashboard schema bump"
    );
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

/// When the dashboard section is dropped because of a version
/// mismatch, any subsequent save must not lose unrelated cached
/// quotas. Otherwise a stale dashboard schema would silently force a
/// quota re-fetch on the next save path.
#[test]
fn save_after_version_mismatch_preserves_quotas() {
    let dir = TempDir::new().unwrap();
    let v3_payload = serde_json::json!({
        "version": 3,
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
        serde_json::to_string(&v3_payload).unwrap(),
    )
    .unwrap();

    // Saving a fresh dashboard should rewrite the file at v4 while
    // carrying the existing quota section forward untouched.
    save_dashboard_at(dir.path(), &sample_entries()).unwrap();

    let loaded = load_at(dir.path());
    assert!(loaded.dashboard.is_some(), "v4 dashboard rewritten on save");
    let quotas = loaded
        .quotas
        .expect("v3 quota section must survive the dashboard rewrite");
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
    assert_eq!(entry.ipbr_match_key, None);
    assert_eq!(entry.route_underlying_vendor, None);
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
fn version_mismatch_drops_dashboard_only() {
    let dir = TempDir::new().unwrap();
    save_dashboard_at(dir.path(), &sample_entries()).unwrap();
    save_quotas_at(dir.path(), &sample_quotas()).unwrap();
    let path = dir.path().join("models.json");
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    file.version = 999;
    fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

    let loaded = load_at(dir.path());
    assert!(
        loaded.dashboard.is_none(),
        "dashboard payload is dropped on any version mismatch"
    );
    assert!(
        loaded.quotas.is_some(),
        "quota section is independent of dashboard schema version"
    );
}

#[test]
fn v2_cache_file_drops_dashboard_only() {
    let dir = TempDir::new().unwrap();
    save_dashboard_at(dir.path(), &sample_entries()).unwrap();
    save_quotas_at(dir.path(), &sample_quotas()).unwrap();
    let path = dir.path().join("models.json");
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    file.version = 2;
    fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

    let loaded = load_at(dir.path());
    assert!(loaded.dashboard.is_none());
    assert!(
        loaded.quotas.is_some(),
        "v2 quota section keeps loading under the existing TTL"
    );
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
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
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
    let mut file: CacheFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
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
