use super::*;
use crate::cache::{DashboardEntry, LoadedCache, LoadedSection, QuotaPayload, ResetPayload};

fn all_vendors() -> BTreeSet<VendorKind> {
    [
        VendorKind::Codex,
        VendorKind::Claude,
        VendorKind::Gemini,
        VendorKind::Kimi,
    ]
    .into_iter()
    .collect()
}

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
        ipbr_match_key: None,
        route_underlying_vendor: None,
        route_provider: None,
        display_order,
        fallback_from: None,
    }
}

fn make_ipbr_entry(name: &str, vendor: &str, match_key: &str) -> DashboardEntry {
    let mut entry = make_entry(name, vendor, 80.0, 78.0);
    entry.score_source = ScoreSource::Ipbr;
    entry.ipbr_row_matched = true;
    entry.ipbr_match_key = Some(match_key.to_string());
    entry.route_underlying_vendor = (vendor == "opencode").then_some(VendorKind::Claude);
    entry
}

fn opencode_available() -> BTreeSet<VendorKind> {
    BTreeSet::from([VendorKind::Claude, VendorKind::Opencode])
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

/// Pure-side wrapper: pulls already-resolved snapshots out of `LoadedCache`
/// and feeds them to `assemble_universe` for the all-vendors policy used by
/// most fixture tests.
fn assemble_from_cache(loaded: LoadedCache) -> Vec<CachedModel> {
    assemble_from_cache_with_available(loaded, &all_vendors())
}

fn assemble_from_cache_with_available(
    loaded: LoadedCache,
    available: &BTreeSet<VendorKind>,
) -> Vec<CachedModel> {
    let dashboard = loaded
        .dashboard
        .map(|section| section.data)
        .unwrap_or_default();
    let quotas = loaded
        .quotas
        .map(|section| section.data)
        .unwrap_or_default();
    let resets = loaded
        .quota_resets
        .map(|section| section.data)
        .unwrap_or_default();
    assemble_universe(dashboard, quotas, resets, available)
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

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

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

    let models = assemble_from_cache(loaded_cache_with_resets(dashboard, quotas, resets));

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
fn assemble_omits_models_with_unknown_vendor() {
    let dashboard = vec![make_entry("unknown-model", "aliens", 90.0, 90.0)];
    let quotas = make_quota_payload(&[]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

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

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    // Canonical surfaces under its real name (auto-inferred from the picked
    // row), not a synthesized "kimi-latest" placeholder.
    assert_eq!(models[0].name, "kimi-k2");
    assert_eq!(models[0].vendor, VendorKind::Kimi);
    // Retains the lower-display-order model's cosmetic score (70.0),
    // proving overall_score did not drive the collapse.
    assert_eq!(models[0].overall_score, 70.0);
}

#[test]
fn assemble_collapses_kimi_prefers_higher_ipbr_score_over_display_order() {
    // Real-feed scenario the previous display-order-only collapse mishandled:
    // the inventory feed lists a weaker model ahead of the stronger one
    // (lower display_order) but ipbr's phase scores show the latter is the
    // canonical pick. The collapse MUST keep the better ipbr scores so
    // downstream phase-rank / pool-weight cells reflect the canonical kimi.
    let mut weaker_low_order =
        make_entry_with_order("kimi-k2-0905-preview", "moonshotai", 38.0, 38.0, 14);
    weaker_low_order.score_source = crate::selection::ScoreSource::Ipbr;
    weaker_low_order.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        idea: Some(22.8),
        planning: Some(28.0),
        build: Some(52.2),
        review: Some(48.6),
    };

    let mut stronger_high_order = make_entry_with_order("kimi-real", "moonshotai", 73.0, 73.0, 15);
    stronger_high_order.score_source = crate::selection::ScoreSource::Ipbr;
    stronger_high_order.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        idea: Some(69.0),
        planning: Some(69.4),
        build: Some(73.6),
        review: Some(80.1),
    };

    let dashboard = vec![weaker_low_order, stronger_high_order];
    let quotas = make_quota_payload(&[("moonshotai", "kimi-k2-0905-preview", Some(90))]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    let kimi = &models[0];
    // Auto-inferred name: the picked entry's actual name surfaces.
    assert_eq!(kimi.name, "kimi-real");
    // The retained model must be the one with the higher ipbr phase-score
    // sum, even though its display_order is later in the inventory feed.
    assert_eq!(kimi.ipbr_phase_scores.build, Some(73.6));
    assert_eq!(kimi.ipbr_phase_scores.review, Some(80.1));
    assert_eq!(kimi.score_source, crate::selection::ScoreSource::Ipbr);
}

#[test]
fn assemble_collapses_kimi_ignores_cosmetic_overall_score() {
    // overall_score / current_score are display-only summaries — they MUST
    // NOT win against another sibling whose ipbr phase scores are stronger.
    // Here the higher-overall-score kimi has *lower* ipbr scores, so the
    // ipbr-favored row must be retained.
    let mut weak_ipbr_high_overall =
        make_entry_with_order("kimi-k1.5", "moonshotai", 95.0, 93.0, 0);
    weak_ipbr_high_overall.score_source = crate::selection::ScoreSource::Ipbr;
    weak_ipbr_high_overall.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(50.0),
        review: Some(48.0),
        ..Default::default()
    };

    let mut strong_ipbr_low_overall = make_entry_with_order("kimi-k2", "moonshotai", 60.0, 58.0, 5);
    strong_ipbr_low_overall.score_source = crate::selection::ScoreSource::Ipbr;
    strong_ipbr_low_overall.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(80.0),
        review: Some(78.0),
        ..Default::default()
    };

    let dashboard = vec![weak_ipbr_high_overall, strong_ipbr_low_overall];
    let quotas = make_quota_payload(&[("moonshotai", "kimi-k2", Some(90))]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    let kimi = &models[0];
    // Auto-inferred name: the strong-ipbr kimi-k2 row wins and surfaces
    // under its own name.
    assert_eq!(kimi.name, "kimi-k2");
    // overall_score 95 lost to overall_score 60 because ipbr phase scores
    // were stronger on the kimi-k2 row.
    assert_eq!(kimi.overall_score, 60.0);
    assert_eq!(kimi.ipbr_phase_scores.build, Some(80.0));
    assert_eq!(kimi.ipbr_phase_scores.review, Some(78.0));
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

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    let kimi = &models[0];
    assert_eq!(kimi.name, "kimi-k2");
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

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

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

    let models =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

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

    let models =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].vendor, VendorKind::Claude);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn stale_on_error_fallback_uses_expired_dashboard() {
    // Fresh (non-expired) dashboard should be used directly without fetching
    let loaded = LoadedCache {
        dashboard: Some(LoadedSection {
            data: vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)],
            expired: false,
        }),
        quotas: Some(LoadedSection {
            data: make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]),
            expired: false,
        }),
        quota_resets: Some(LoadedSection {
            data: make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]),
            expired: false,
        }),
    };

    let models = assemble_from_cache(loaded);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn fresh_cache_with_empty_dashboard_returns_empty() {
    let loaded = LoadedCache {
        dashboard: Some(LoadedSection {
            data: Vec::new(),
            expired: false,
        }),
        quotas: Some(LoadedSection {
            data: make_quota_payload(&[("claude", "claude-sonnet", Some(80))]),
            expired: false,
        }),
        quota_resets: Some(LoadedSection {
            data: make_reset_payload(&[("claude", "claude-sonnet", None)]),
            expired: false,
        }),
    };

    let models = assemble_from_cache(loaded);

    assert!(models.is_empty());
}

#[test]
fn assemble_universe_uses_provided_snapshot_without_reloading() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    let resets = empty_resets_for_quotas(&quotas);

    let models = assemble_universe(dashboard, quotas, resets, &all_vendors());

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn quota_heuristic_fallback_when_no_exact_match() {
    let dashboard = vec![make_entry("claude-opus-4-7", "claude", 90.0, 88.0)];
    // Quota exists for a different claude model
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(75))]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    // Should get quota via heuristic (Claude models share quota)
    assert_eq!(models[0].quota_percent, Some(75));
}

#[test]
fn dedup_keeps_direct_when_quota_meets_floor() {
    // Direct quota at or above the 20% floor wins unconditionally — even
    // when opencode reports more remaining headroom.
    for (direct_quota, opencode_quota) in [
        (Some(20), Some(99)),
        (Some(50), Some(50)),
        (Some(80), Some(99)),
        (Some(80), None),
        (Some(20), None),
    ] {
        let survivor = run_dedup(direct_quota, opencode_quota);
        assert_eq!(survivor.vendor, VendorKind::Claude);
        assert_eq!(survivor.name, "claude-opus-4-7");
        assert_eq!(survivor.quota_percent, direct_quota);
    }
}

#[test]
fn dedup_falls_back_to_opencode_when_direct_below_floor_and_opencode_higher() {
    // Direct quota below 20% (or unknown) defers to opencode whenever
    // opencode reports a strictly higher remaining quota.
    for (direct_quota, opencode_quota) in [
        (Some(19), Some(50)),
        (Some(0), Some(1)),
        (None, Some(80)),
        (None, Some(0)),
    ] {
        let survivor = run_dedup(direct_quota, opencode_quota);
        assert_eq!(survivor.vendor, VendorKind::Opencode);
        assert_eq!(survivor.name, "opencode/claude-opus-4-7");
        assert_eq!(survivor.quota_percent, opencode_quota);
    }
}

#[test]
fn dedup_keeps_direct_below_floor_when_opencode_does_not_win() {
    // Direct below the floor still wins on ties, when its quota is
    // strictly higher than opencode's, or when both sides are unknown.
    for (direct_quota, opencode_quota) in [
        (Some(19), Some(19)),
        (Some(19), Some(10)),
        (Some(19), None),
        (None, None),
        (Some(0), Some(0)),
    ] {
        let survivor = run_dedup(direct_quota, opencode_quota);
        assert_eq!(survivor.vendor, VendorKind::Claude);
        assert_eq!(survivor.name, "claude-opus-4-7");
        assert_eq!(survivor.quota_percent, direct_quota);
    }
}

fn run_dedup(direct_quota: Option<u8>, opencode_quota: Option<u8>) -> CachedModel {
    let direct = make_ipbr_entry("claude-opus-4-7", "claude", "claude-opus-4-7");
    let mut routed = direct.clone();
    routed.name = "opencode/claude-opus-4-7".to_string();
    routed.vendor = "opencode".to_string();
    routed.route_underlying_vendor = Some(VendorKind::Claude);
    let quotas = make_quota_payload(&[
        ("claude", "claude-opus-4-7", direct_quota),
        ("opencode", "opencode/claude-opus-4-7", opencode_quota),
    ]);
    let models = assemble_universe(
        vec![direct, routed],
        quotas,
        BTreeMap::new(),
        &opencode_available(),
    );
    assert_eq!(models.len(), 1);
    models.into_iter().next().unwrap()
}

#[test]
fn opencode_ipbr_matched_inventory_renders_with_unknown_quota() {
    let routed = make_ipbr_entry("opencode/claude-opus-4-7", "opencode", "claude-opus-4-7");

    let models = assemble_universe(
        vec![routed],
        BTreeMap::new(),
        BTreeMap::new(),
        &opencode_available(),
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].vendor, VendorKind::Opencode);
    assert_eq!(models[0].quota_percent, None);
    assert_eq!(models[0].ipbr_match_key.as_deref(), Some("claude-opus-4-7"));
    assert_eq!(models[0].route_underlying_vendor, Some(VendorKind::Claude));
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

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

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
