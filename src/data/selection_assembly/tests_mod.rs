use super::*;
use crate::cache::{self, DashboardEntry, LoadedCache, LoadedSection, QuotaPayload, ResetPayload};
use std::collections::BTreeMap;

fn make_entry(name: &str, vendor: &str, overall: f64, current: f64) -> DashboardEntry {
    DashboardEntry {
        vendor: vendor.to_string(),
        name: name.to_string(),
        overall_score: overall,
        current_score: current,
        standard_error: 2.0,
        axes: vec![
            ("codequality".to_string(), 0.85),
            ("correctness".to_string(), 0.85),
            ("debugging".to_string(), 0.85),
            ("safety".to_string(), 0.85),
        ],
        axis_provenance: BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        ipbr_match_key: None,
        route_underlying_vendor: None,
        route_provider: None,
        display_order: 0,
        fallback_from: None,
    }
}

fn make_quota_payload(entries: &[(&str, &str, Option<u8>)]) -> QuotaPayload {
    let mut payload: QuotaPayload = BTreeMap::new();
    for (vendor, name, quota) in entries {
        payload
            .entry(vendor.to_string())
            .or_default()
            .insert(name.to_string(), *quota);
    }
    payload
}

fn make_reset_payload(entries: &[(&str, &str, Option<&str>)]) -> ResetPayload {
    let mut payload: ResetPayload = BTreeMap::new();
    for (vendor, name, reset) in entries {
        payload.entry(vendor.to_string()).or_default().insert(
            name.to_string(),
            reset.map(|value| {
                chrono::DateTime::parse_from_rfc3339(value)
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            }),
        );
    }
    payload
}

fn empty_resets_for_quotas(quotas: &QuotaPayload) -> ResetPayload {
    quotas
        .iter()
        .map(|(vendor, models)| {
            (
                vendor.clone(),
                models.keys().map(|name| (name.clone(), None)).collect(),
            )
        })
        .collect()
}

fn loaded_cache_with(dashboard: Vec<DashboardEntry>, quotas: QuotaPayload) -> LoadedCache {
    let resets = empty_resets_for_quotas(&quotas);
    LoadedCache {
        dashboard: Some(LoadedSection {
            data: dashboard,
            expired: false,
        }),
        quotas: Some(LoadedSection {
            data: quotas,
            expired: false,
        }),
        quota_resets: Some(LoadedSection {
            data: resets,
            expired: false,
        }),
    }
}

fn loaded_cache_with_resets(
    dashboard: Vec<DashboardEntry>,
    quotas: QuotaPayload,
    resets: ResetPayload,
) -> LoadedCache {
    LoadedCache {
        dashboard: Some(LoadedSection {
            data: dashboard,
            expired: false,
        }),
        quotas: Some(LoadedSection {
            data: quotas,
            expired: false,
        }),
        quota_resets: Some(LoadedSection {
            data: resets,
            expired: false,
        }),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn assemble_refreshes_when_cached_reset_coverage_is_partial() {
    let dashboard = vec![
        make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
        make_entry("claude-opus-4-1", "claude", 84.0, 81.0),
    ];
    let quotas = make_quota_payload(&[
        ("claude", "claude-sonnet-4-6", Some(80)),
        ("claude", "claude-opus-4-1", Some(80)),
    ]);
    let resets = make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]);
    let available = BTreeSet::from([VendorKind::Claude]);
    let temp = tempfile::TempDir::new().unwrap();
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let claude_path = bin_dir.join("claude");
    let security_path = bin_dir.join("security");
    std::fs::write(
        &claude_path,
        "#!/bin/sh\nif [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then\n  printf '{\"orgId\":\"test-org\"}'\n  exit 0\nfi\nsleep 1\n",
    )
    .unwrap();
    std::fs::write(&security_path, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&security_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let original_path = std::env::var_os("PATH");

    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        let mut paths = vec![bin_dir.clone()];
        if let Some(value) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&value));
        }
        let joined = std::env::join_paths(paths).unwrap();
        std::env::set_var("PATH", joined);
    }

    let (models, errors) = assemble_with_refresh(
        loaded_cache_with_resets(dashboard, quotas, resets),
        &available,
    )
    .await;

    unsafe {
        match original_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }

    assert_eq!(models.len(), 2);
    assert_eq!(errors.len(), 1, "partial reset gaps should trigger refresh");
    assert_eq!(errors[0].vendor, VendorKind::Claude);
}

#[test]
fn assemble_from_loaded_uses_acp_configured_vendor_availability() {
    let loaded = loaded_cache_with(
        vec![
            make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
            make_entry("gpt-5.5", "openai", 80.0, 78.0),
        ],
        make_quota_payload(&[
            ("claude", "claude-sonnet-4-6", Some(80)),
            ("openai", "gpt-5.5", Some(70)),
        ]),
    );
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original_available = std::env::var_os("CODEXIZE_TEST_AVAILABLE_VENDORS");
    let original_claude = std::env::var_os("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM");
    let original_codex = std::env::var_os("CODEXIZE_TEST_ACP_CODEX_PROGRAM");
    let original_gemini = std::env::var_os("CODEXIZE_TEST_ACP_GEMINI_PROGRAM");
    let original_kimi = std::env::var_os("CODEXIZE_TEST_ACP_KIMI_PROGRAM");
    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        std::env::set_var("CODEXIZE_TEST_AVAILABLE_VENDORS", "claude");
        std::env::set_var(
            "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
            "/definitely/missing/claude",
        );
        std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", "/bin/sh");
        std::env::set_var(
            "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
            "/definitely/missing/gemini",
        );
        std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", "/definitely/missing/kimi");
    }

    let outcome = std::panic::catch_unwind(|| assemble_from_loaded(&loaded));

    unsafe {
        match original_available {
            Some(value) => std::env::set_var("CODEXIZE_TEST_AVAILABLE_VENDORS", value),
            None => std::env::remove_var("CODEXIZE_TEST_AVAILABLE_VENDORS"),
        }
        match original_claude {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM"),
        }
        match original_codex {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM"),
        }
        match original_gemini {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM"),
        }
        match original_kimi {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM"),
        }
    }

    let models = outcome.expect("assemble_from_loaded should not panic");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].vendor, VendorKind::Codex);
    assert_eq!(models[0].name, "gpt-5.5");
}

fn with_temp_home_cache<T>(
    dashboard: Vec<DashboardEntry>,
    quotas: QuotaPayload,
    f: impl FnOnce() -> T,
) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::TempDir::new().unwrap();
    let original = std::env::var_os("HOME");
    let original_claude = std::env::var_os("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM");
    let original_codex = std::env::var_os("CODEXIZE_TEST_ACP_CODEX_PROGRAM");
    let original_gemini = std::env::var_os("CODEXIZE_TEST_ACP_GEMINI_PROGRAM");
    let original_kimi = std::env::var_os("CODEXIZE_TEST_ACP_KIMI_PROGRAM");
    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        std::env::set_var("HOME", temp.path());
        std::env::set_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM", "/bin/sh");
        std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", "/bin/sh");
        std::env::set_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM", "/bin/sh");
        std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", "/bin/sh");
    }
    cache::save_dashboard(&dashboard).unwrap();
    cache::save_quotas(&quotas).unwrap();
    cache::save_quota_resets(&empty_resets_for_quotas(&quotas)).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match original {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_claude {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM"),
        }
        match original_codex {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM"),
        }
        match original_gemini {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM"),
        }
        match original_kimi {
            Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", value),
            None => std::env::remove_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM"),
        }
    }
    outcome.unwrap()
}

#[test]
fn assemble_models_uses_default_cache_dir_when_fresh() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    with_temp_home_cache(dashboard, quotas, || {
        // Cache was just written, so dashboard + quotas are fresh; the
        // async loader should not need any network refresh.
        let (models, errors) = crate::data::async_bridge::block_on_io(assemble_models_async());
        assert!(
            errors.is_empty(),
            "fresh cache should not trigger refresh errors"
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "claude-sonnet-4-6");
        assert_eq!(models[0].quota_percent, Some(80));
    });
}

#[test]
fn assemble_from_cached_only_returns_empty_when_no_cache() {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::TempDir::new().unwrap();
    let original = std::env::var_os("HOME");
    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        std::env::set_var("HOME", temp.path());
    }
    let models = assemble_from_cached_only();
    unsafe {
        match original {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
    assert!(models.is_empty(), "no cache should yield empty model list");
}

#[test]
fn assemble_from_cached_only_yields_models_when_cache_is_present() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    with_temp_home_cache(dashboard, quotas, || {
        let models = assemble_from_cached_only();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "claude-sonnet-4-6");
        assert_eq!(models[0].quota_percent, Some(80));
    });
}
