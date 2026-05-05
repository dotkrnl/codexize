use super::*;
use crate::cache::DashboardEntry;

fn make_entry(name: &str, vendor: &str, overall: f64, current: f64) -> DashboardEntry {
    make_entry_with_order(name, vendor, overall, current, 0)
}

fn make_entry_with_order(
    name: &str,
    vendor: &str,
    overall: f64,
    current: f64,
    display_order: usize,
) -> DashboardEntry {
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
        display_order,
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
        dashboard: Some(cache::LoadedSection {
            data: dashboard,
            expired: false,
        }),
        quotas: Some(cache::LoadedSection {
            data: quotas,
            expired: false,
        }),
        quota_resets: Some(cache::LoadedSection {
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
        dashboard: Some(cache::LoadedSection {
            data: dashboard,
            expired: false,
        }),
        quotas: Some(cache::LoadedSection {
            data: quotas,
            expired: false,
        }),
        quota_resets: Some(cache::LoadedSection {
            data: resets,
            expired: false,
        }),
    }
}

#[test]
fn assemble_merges_dashboard_and_quotas() {
    let mut claude_entry = make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0);
    claude_entry
        .axis_provenance
        .insert("correctness".to_string(), "suite:hourly".to_string());
    let dashboard = vec![claude_entry, make_entry("gpt-5.5", "openai", 80.0, 78.0)];
    let quotas = make_quota_payload(&[
        ("claude", "claude-sonnet-4-6", Some(80)),
        ("openai", "gpt-5.5", Some(70)),
    ]);

    let (models, errors) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert!(errors.is_empty());
    assert_eq!(models.len(), 2);
    let claude = models
        .iter()
        .find(|m| m.name == "claude-sonnet-4-6")
        .unwrap();
    assert_eq!(claude.vendor, VendorKind::Claude);
    assert_eq!(claude.quota_percent, Some(80));
    assert_eq!(
        claude
            .axis_provenance
            .get("correctness")
            .map(String::as_str),
        Some("suite:hourly")
    );
    let codex = models.iter().find(|m| m.name == "gpt-5.5").unwrap();
    assert_eq!(codex.vendor, VendorKind::Codex);
    assert_eq!(codex.quota_percent, Some(70));
}

#[test]
fn assemble_merges_cached_quota_resets() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    let resets =
        make_reset_payload(&[("claude", "claude-sonnet-4-6", Some("2026-04-30T12:00:00Z"))]);

    let (models, errors) =
        assemble_from_cache(loaded_cache_with_resets(dashboard, quotas, resets));

    assert!(errors.is_empty());
    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].quota_resets_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-30T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        )
    );
}

#[test]
fn assemble_refreshes_when_cached_reset_coverage_is_partial() {
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
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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
        std::fs::set_permissions(&security_path, std::fs::Permissions::from_mode(0o755))
            .unwrap();
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

    let (models, errors) = assemble_from_cache_with_available(
        loaded_cache_with_resets(dashboard, quotas, resets),
        &available,
    );

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
fn assemble_omits_models_with_unknown_vendor() {
    let dashboard = vec![make_entry("unknown-model", "aliens", 90.0, 90.0)];
    let quotas = make_quota_payload(&[]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert!(models.is_empty());
}

#[test]
fn assemble_collapses_kimi_models() {
    // Stable inventory order (display_order) decides the canonical
    // representative, not cosmetic overall_score.
    let dashboard = vec![
        // Lower display_order (1) but lower overall_score — should win.
        make_entry_with_order("kimi-k2", "moonshotai", 70.0, 68.0, 1),
        // Higher display_order (5) but higher overall_score — should lose.
        make_entry_with_order("kimi-k1.5", "moonshotai", 75.0, 73.0, 5),
    ];
    let quotas = make_quota_payload(&[
        ("moonshotai", "kimi-k2", Some(90)),
        ("moonshotai", "kimi-k1.5", Some(90)),
    ]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "kimi-latest");
    assert_eq!(models[0].vendor, VendorKind::Kimi);
    // Retains the lower-display-order model's cosmetic score (70.0),
    // proving overall_score did not drive the collapse.
    assert_eq!(models[0].overall_score, 70.0);
}

#[test]
fn assemble_collapses_kimi_by_display_order_not_overall_score() {
    // Explicit conflict: the model with the higher overall_score has a
    // worse (higher) display_order. The collapse MUST NOT use
    // overall_score as a proxy for rank or selection authority.
    let mut low_order = make_entry_with_order("kimi-k2", "moonshotai", 60.0, 58.0, 0);
    low_order.score_source = crate::selection::ScoreSource::Ipbr;
    low_order.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(80.0),
        review: Some(78.0),
        ..Default::default()
    };

    let mut high_order = make_entry_with_order("kimi-k1.5", "moonshotai", 95.0, 93.0, 10);
    high_order.score_source = crate::selection::ScoreSource::Ipbr;
    high_order.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(50.0),
        review: Some(48.0),
        ..Default::default()
    };

    let dashboard = vec![low_order, high_order];
    let quotas = make_quota_payload(&[("moonshotai", "kimi-k2", Some(90))]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    let kimi = &models[0];
    assert_eq!(kimi.name, "kimi-latest");
    // The retained model must be the one with lower display_order,
    // regardless of its lower overall_score.
    assert_eq!(kimi.overall_score, 60.0);
    // Its ipbr phase scores must survive collapse so downstream
    // selection uses them, not the high-overall-score model's scores.
    assert_eq!(kimi.ipbr_phase_scores.build, Some(80.0));
    assert_eq!(kimi.ipbr_phase_scores.review, Some(78.0));
    assert_eq!(kimi.score_source, crate::selection::ScoreSource::Ipbr);
}

#[test]
fn assemble_collapsed_kimi_selection_uses_ipbr_phase_scores() {
    use crate::selection::config::SelectionPhase;
    use crate::selection::ranking::phase_rank_score;

    let mut entry = make_entry_with_order("kimi-k2", "moonshotai", 70.0, 68.0, 0);
    entry.score_source = crate::selection::ScoreSource::Ipbr;
    entry.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(82.0),
        review: Some(79.0),
        ..Default::default()
    };

    let dashboard = vec![entry];
    let quotas = make_quota_payload(&[("moonshotai", "kimi-k2", Some(90))]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    let kimi = &models[0];
    assert_eq!(kimi.name, "kimi-latest");
    // Build and Review auto-selection must see the ipbr phase scores
    // from the collapsed model, not fall back to overall_score.
    assert_eq!(phase_rank_score(kimi, SelectionPhase::Build), Some(82.0));
    assert_eq!(phase_rank_score(kimi, SelectionPhase::Review), Some(79.0));
}

#[test]
fn assemble_synthesizes_missing_sibling() {
    let dashboard = vec![make_entry("gpt-5.4", "openai", 80.0, 78.0)];
    // Quota has gpt-5.5 which is missing from dashboard
    let quotas = make_quota_payload(&[
        ("openai", "gpt-5.4", Some(80)),
        ("openai", "gpt-5.5", Some(70)),
    ]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 2);
    let synthesized = models.iter().find(|m| m.name == "gpt-5.5").unwrap();
    assert_eq!(synthesized.fallback_from.as_deref(), Some("gpt-5.4"));
    assert_eq!(synthesized.quota_percent, Some(70));
}

#[test]
fn unavailable_vendors_are_omitted_before_models_are_returned() {
    let dashboard = vec![
        make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
        make_entry("gpt-5.5", "openai", 80.0, 78.0),
        make_entry("gemini-2.5-pro", "google", 75.0, 73.0),
    ];
    let quotas = make_quota_payload(&[
        ("claude", "claude-sonnet-4-6", Some(80)),
        ("openai", "gpt-5.5", Some(70)),
        ("google", "gemini-2.5-pro", Some(60)),
    ]);
    let available = BTreeSet::from([VendorKind::Codex]);

    let (models, errors) =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

    assert!(errors.is_empty());
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].vendor, VendorKind::Codex);
    assert_eq!(models[0].name, "gpt-5.5");
    assert_eq!(models[0].quota_percent, Some(70));
}

#[test]
fn available_claude_keeps_anthropic_dashboard_entries() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "anthropic", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    let available = BTreeSet::from([VendorKind::Claude]);

    let (models, errors) =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

    assert!(errors.is_empty());
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].vendor, VendorKind::Claude);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
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

#[test]
fn stale_on_error_fallback_uses_expired_dashboard() {
    // Fresh (non-expired) dashboard should be used directly without fetching
    let loaded = LoadedCache {
        dashboard: Some(cache::LoadedSection {
            data: vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)],
            expired: false,
        }),
        quotas: Some(cache::LoadedSection {
            data: make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]),
            expired: false,
        }),
        quota_resets: Some(cache::LoadedSection {
            data: make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]),
            expired: false,
        }),
    };

    let (models, errors) = assemble_from_cache(loaded);

    assert!(errors.is_empty());
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn fresh_cache_with_empty_dashboard_returns_empty() {
    let loaded = LoadedCache {
        dashboard: Some(cache::LoadedSection {
            data: Vec::new(),
            expired: false,
        }),
        quotas: Some(cache::LoadedSection {
            data: make_quota_payload(&[("claude", "claude-sonnet", Some(80))]),
            expired: false,
        }),
        quota_resets: Some(cache::LoadedSection {
            data: make_reset_payload(&[("claude", "claude-sonnet", None)]),
            expired: false,
        }),
    };

    let (models, _) = assemble_from_cache(loaded);

    assert!(models.is_empty());
}

#[test]
fn assemble_from_loaded_uses_provided_snapshot_without_reloading() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    let available = [
        VendorKind::Codex,
        VendorKind::Claude,
        VendorKind::Gemini,
        VendorKind::Kimi,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();

    let models =
        assemble_from_loaded_with_available(&loaded_cache_with(dashboard, quotas), &available);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn quota_heuristic_fallback_when_no_exact_match() {
    let dashboard = vec![make_entry("claude-opus-4-7", "claude", 90.0, 88.0)];
    // Quota exists for a different claude model
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(75))]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    // Should get quota via heuristic (Claude models share quota)
    assert_eq!(models[0].quota_percent, Some(75));
}

#[test]
fn merge_preserves_expired_vendor_on_error() {
    // Cached has data for all four vendors.
    let mut cached: QuotaPayload = BTreeMap::new();
    cached.insert(
        "claude".to_string(),
        BTreeMap::from([("claude-sonnet".to_string(), Some(50))]),
    );
    cached.insert(
        "openai".to_string(),
        BTreeMap::from([("gpt-5".to_string(), Some(60))]),
    );
    cached.insert(
        "google".to_string(),
        BTreeMap::from([("gemini-2.5-pro".to_string(), Some(70))]),
    );

    // Fresh refresh succeeded only for Claude.
    let mut fresh: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
    fresh.insert(
        VendorKind::Claude,
        BTreeMap::from([("claude-sonnet".to_string(), Some(80))]),
    );

    let merged = merge_quota_payload(&cached, fresh);

    // Claude was refreshed → fresh value wins.
    assert_eq!(
        merged
            .get("claude")
            .and_then(|m| m.get("claude-sonnet").copied()),
        Some(Some(80))
    );
    // OpenAI/Google failed to refresh → expired cached values preserved.
    assert_eq!(
        merged.get("openai").and_then(|m| m.get("gpt-5").copied()),
        Some(Some(60))
    );
    assert_eq!(
        merged
            .get("google")
            .and_then(|m| m.get("gemini-2.5-pro").copied()),
        Some(Some(70))
    );
}

#[test]
fn merge_overlays_when_cached_uses_alias_key() {
    // Cached used the str_to_vendor alias ("codex") rather than vendor_kind_to_str ("openai").
    let mut cached: QuotaPayload = BTreeMap::new();
    cached.insert(
        "codex".to_string(),
        BTreeMap::from([("gpt-5".to_string(), Some(40))]),
    );

    let mut fresh: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
    fresh.insert(
        VendorKind::Codex,
        BTreeMap::from([("gpt-5".to_string(), Some(90))]),
    );

    let merged = merge_quota_payload(&cached, fresh);

    // The alias entry is dropped (its vendor was refreshed) and the canonical
    // "openai" key carries the fresh value.
    assert!(!merged.contains_key("codex"));
    assert_eq!(
        merged.get("openai").and_then(|m| m.get("gpt-5").copied()),
        Some(Some(90))
    );
}

#[test]
fn merge_keeps_unknown_vendor_keys() {
    let mut cached: QuotaPayload = BTreeMap::new();
    cached.insert(
        "aliens".to_string(),
        BTreeMap::from([("ufo-9000".to_string(), Some(33))]),
    );

    let merged = merge_quota_payload(&cached, BTreeMap::new());

    assert_eq!(
        merged
            .get("aliens")
            .and_then(|m| m.get("ufo-9000").copied()),
        Some(Some(33))
    );
}

#[test]
fn missing_quota_results_in_none() {
    let dashboard = vec![make_entry("gemini-2.5-pro", "google", 85.0, 83.0)];
    let quotas = make_quota_payload(&[]);

    let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].quota_percent, None);
}

#[test]
fn reset_coverage_gaps_require_matching_model_keys() {
    let quotas = make_quota_payload(&[
        ("claude", "claude-sonnet-4-6", Some(80)),
        ("claude", "claude-opus-4-1", Some(80)),
    ]);
    let partial_resets = make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]);
    let covered_resets = make_reset_payload(&[
        ("claude", "claude-sonnet-4-6", None),
        ("claude", "claude-opus-4-1", None),
    ]);

    assert!(has_reset_coverage_gaps(&quotas, &partial_resets));
    assert!(!has_reset_coverage_gaps(&quotas, &covered_resets));
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
        // public wrapper should not need any network refresh.
        let (models, errors) = assemble_models();
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
fn score_failure_prefers_cached_ipbr_entries_over_inventory_only() {
    let mut cached_with_ipbr = make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0);
    cached_with_ipbr.score_source = ScoreSource::Ipbr;
    cached_with_ipbr.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        idea: Some(70.0),
        planning: Some(72.0),
        build: Some(73.0),
        review: Some(71.0),
    };
    let cached = vec![cached_with_ipbr];
    let inv_only = vec![make_entry("claude-sonnet-4-6", "claude", 0.0, 0.0)];

    let resolved = resolve_score_failure_entries(cached.clone(), inv_only);

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].score_source, ScoreSource::Ipbr);
    assert_eq!(resolved[0].ipbr_phase_scores.build, Some(73.0));
}

#[test]
fn score_failure_falls_back_to_inventory_only_when_no_cached_ipbr() {
    // No cached row carries `ScoreSource::Ipbr`, so the inventory-only
    // refresh must still surface so the strip is not blank — phase
    // scores stay `None` until ipbr recovers.
    let cached = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
    let inv_only = vec![make_entry("gpt-5.4", "openai", 0.0, 0.0)];

    let resolved = resolve_score_failure_entries(cached, inv_only);

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "gpt-5.4");
    assert_eq!(resolved[0].score_source, ScoreSource::None);
}

#[test]
fn score_failure_falls_back_to_inventory_only_when_cache_is_empty() {
    let inv_only = vec![make_entry("claude-sonnet-4-6", "claude", 0.0, 0.0)];

    let resolved = resolve_score_failure_entries(Vec::new(), inv_only);

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "claude-sonnet-4-6");
}

#[test]
fn dashboard_warnings_are_exposed_as_refresh_diagnostics() {
    let errors =
        dashboard_warnings_to_quota_errors(vec!["ipbr normalized key 'x' collided".into()]);

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].vendor, VendorKind::Claude);
    assert_eq!(
        errors[0].message,
        "dashboard warning: ipbr normalized key 'x' collided"
    );
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
