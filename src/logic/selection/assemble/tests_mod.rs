use super::*;
use crate::cache::{DashboardEntry, LoadedCache, LoadedSection, QuotaPayload, ResetPayload};
use crate::selection::ScoreSource;

fn candidate(
    subscription: SubscriptionKind,
    cli: CliKind,
    launch_name: &str,
    quota_percent: Option<u8>,
    display_order: usize,
) -> Candidate {
    // Tests in this module exercise the priority ladder via the
    // `enabled / free / official / quota_disabled` axis. By default
    // every fixture candidate is treated as a non-free, non-official,
    // non-quota_disabled provider — i.e. pool `N` — so callers can
    // flip individual flags to land in the pool they're testing.
    Candidate {
        subscription,
        cli,
        launch_name: launch_name.to_string(),
        quota_percent,
        quota_resets_at: None,
        display_order,
        enabled: true,
        free: false,
        // Mirror the baked/provider table convention: anything routed
        // through OpencodeGo is non-official unless a fixture flips it.
        official: !matches!(subscription, SubscriptionKind::OpencodeGo),
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: false,
        effort_eligible: false,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    }
}

#[test]
fn arbitration_prefers_free_at_full_quota_over_lower_direct() {
    let mut free = candidate(
        SubscriptionKind::OpencodeGo,
        CliKind::Opencode,
        "free-opus",
        Some(100),
        1,
    );
    free.free = true;
    free.official = false;
    let candidates = vec![
        candidate(
            SubscriptionKind::Claude,
            CliKind::Claude,
            "claude-opus",
            Some(80),
            0,
        ),
        free,
    ];

    let selected = select_candidate_index(&candidates).unwrap();

    assert_eq!(selected, 1);
}

#[test]
fn arbitration_prefers_free_over_direct_tie_at_full_quota() {
    let mut free = candidate(
        SubscriptionKind::OpencodeGo,
        CliKind::Codex,
        "free-gpt",
        Some(100),
        1,
    );
    free.free = true;
    free.official = false;
    let candidates = vec![
        candidate(
            SubscriptionKind::Codex,
            CliKind::Codex,
            "gpt-5.5",
            Some(100),
            0,
        ),
        free,
    ];

    let selected = select_candidate_index(&candidates).unwrap();

    assert_eq!(selected, 1);
}

#[test]
fn arbitration_keeps_direct_at_floor_over_higher_opencode_go() {
    // Spec §"Selection algorithm" raised the official floor to 21 — at
    // or above that, the official pool wins outright even when a
    // non-official provider (here OpencodeGo) reports more headroom.
    let candidates = vec![
        candidate(
            SubscriptionKind::Claude,
            CliKind::Claude,
            "claude-opus",
            Some(21),
            0,
        ),
        candidate(
            SubscriptionKind::OpencodeGo,
            CliKind::Opencode,
            "claude-opus",
            Some(90),
            1,
        ),
    ];

    let selected = select_candidate_index(&candidates).unwrap();

    assert_eq!(selected, 0);
}

#[test]
fn arbitration_allows_opencode_go_when_direct_below_floor() {
    // Below the 21% official floor the spec merges official ∪
    // non-official and picks by raw effective quota.
    let candidates = vec![
        candidate(
            SubscriptionKind::Claude,
            CliKind::Claude,
            "claude-opus",
            Some(20),
            0,
        ),
        candidate(
            SubscriptionKind::OpencodeGo,
            CliKind::Opencode,
            "claude-opus",
            Some(90),
            1,
        ),
    ];

    let selected = select_candidate_index(&candidates).unwrap();

    assert_eq!(selected, 1);
}

#[test]
fn arbitration_returns_none_for_zero_candidates() {
    assert_eq!(select_candidate_index(&[]), None);
}

#[test]
fn arbitration_uses_display_order_then_launch_name_tiebreakers() {
    let by_order = vec![
        candidate(
            SubscriptionKind::Codex,
            CliKind::Codex,
            "b-model",
            Some(90),
            2,
        ),
        candidate(
            SubscriptionKind::Gemini,
            CliKind::Gemini,
            "a-model",
            Some(90),
            1,
        ),
    ];
    assert_eq!(select_candidate_index(&by_order), Some(1));

    let by_launch_name = vec![
        candidate(
            SubscriptionKind::Codex,
            CliKind::Codex,
            "b-model",
            Some(90),
            1,
        ),
        candidate(
            SubscriptionKind::Gemini,
            CliKind::Gemini,
            "a-model",
            Some(90),
            1,
        ),
    ];
    assert_eq!(select_candidate_index(&by_launch_name), Some(1));
}

#[test]
fn assemble_universe_builds_one_row_per_ipbr_name_with_all_candidates() {
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let mut direct = make_ipbr_entry("claude-opus-4.7", "claude", "claude-opus-4.7");
    direct.display_order = 0;
    let mut routed = make_ipbr_entry("claude-opus-4.7", "opencode", "claude-opus-4.7");
    routed.display_order = 1;
    let dashboard = vec![direct, routed];
    let quotas = make_quota_payload(&[
        ("claude", "claude-shared", Some(70)),
        ("opencode-go", "claude-opus-4.7", Some(95)),
    ]);
    // The baked table carries the canonical Claude provider; the
    // operator's `[[providers]]` list adds an opencode-routed
    // alternative whose `model` still matches the canonical row.
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "claude-opus-4.7".to_string(),
        model: "claude-opus-4.7".to_string(),
        subscription: SubscriptionKind::OpencodeGo,
        enabled: true,
        free: false,
        official: false,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: true,
        effort_eligible: false,
        effort_mapping: EffortMapping::default(),
        quota_lookup_key: None,
        display_order: 1,
    }];
    let available = BTreeSet::from([CliKind::Claude, CliKind::Opencode]);

    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &available, &providers);

    assert_eq!(models.len(), 1);
    let row = &models[0];
    assert_eq!(row.name, "claude-opus-4.7");
    assert_eq!(row.candidates.len(), 2);
    assert!(
        row.candidates
            .iter()
            .any(|candidate| candidate.subscription == SubscriptionKind::OpencodeGo)
    );
    assert!(
        row.candidates
            .iter()
            .any(|candidate| candidate.subscription == SubscriptionKind::Claude)
    );
}

fn all_clis() -> BTreeSet<CliKind> {
    [
        CliKind::Codex,
        CliKind::Claude,
        CliKind::Gemini,
        CliKind::Kimi,
    ]
    .into_iter()
    .collect()
}

fn make_entry(name: &str, _vendor: &str) -> DashboardEntry {
    make_entry_with_order(name, _vendor, 0)
}

fn make_entry_with_order(name: &str, _vendor: &str, display_order: usize) -> DashboardEntry {
    DashboardEntry {
        name: name.to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        display_order,
    }
}

fn make_ipbr_entry(name: &str, vendor: &str, _match_key: &str) -> DashboardEntry {
    let mut entry = make_entry(name, vendor);
    entry.score_source = ScoreSource::Ipbr;
    entry
}

#[test]
fn ipbr_matched_row_name_uses_display_canonical_not_normalized_key() {
    let dashboard = vec![make_ipbr_entry(
        "claude-opus-4.6",
        "anthropic",
        "claude-opus-4.6",
    )];
    let quotas = make_quota_payload(&[("claude", "claude-shared", Some(80))]);

    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &all_clis(), &[]);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-opus-4.6");
}

fn opencode_available() -> BTreeSet<CliKind> {
    BTreeSet::from([CliKind::Claude, CliKind::Opencode])
}

fn make_quota_payload(entries: &[(&str, &str, Option<u8>)]) -> QuotaPayload {
    let mut payload = QuotaPayload::default();
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
    assemble_from_cache_with_available(loaded, &all_clis())
}

fn assemble_from_cache_with_available(
    loaded: LoadedCache,
    available: &BTreeSet<CliKind>,
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
    let (models, _warnings) = assemble_universe(dashboard, quotas, resets, available, &[]);
    models
}

#[test]
fn assemble_merges_dashboard_and_quotas() {
    let claude_entry = make_entry("claude-sonnet-4.6", "claude");
    let dashboard = vec![claude_entry, make_entry("gpt-5.5", "openai")];
    let quotas = make_quota_payload(&[
        ("claude", "claude-shared", Some(80)),
        ("openai", "gpt-5.5", Some(70)),
    ]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 2);
    let claude = models
        .iter()
        .find(|m| m.name == "claude-sonnet-4.6")
        .unwrap();
    assert_eq!(claude.subscription, SubscriptionKind::Claude);
    assert_eq!(claude.quota_percent, Some(80));
    let codex = models.iter().find(|m| m.name == "gpt-5.5").unwrap();
    assert_eq!(codex.subscription, SubscriptionKind::Codex);
    assert_eq!(codex.quota_percent, Some(70));
}

#[test]
fn assemble_merges_cached_quota_resets() {
    let dashboard = vec![make_entry("claude-sonnet-4.6", "claude")];
    let quotas = make_quota_payload(&[("claude", "claude-shared", Some(80))]);
    let resets = make_reset_payload(&[("claude", "claude-shared", Some("2026-04-30T12:00:00Z"))]);

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
    let dashboard = vec![make_entry("unknown-model", "aliens")];
    let quotas = make_quota_payload(&[]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert!(models.is_empty());
}

#[test]
fn assemble_warns_only_for_provider_models_missing_from_ipbr() {
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let mut dashboard: Vec<DashboardEntry> = crate::logic::selection::baked::BAKED_TABLE
        .iter()
        .map(|row| make_ipbr_entry(row.model, "ipbr", row.model))
        .collect();
    dashboard.push(make_ipbr_entry("ipbr-only-row", "ipbr", "ipbr-only-row"));
    let providers = vec![ProviderEntry {
        cli: CliKind::Claude,
        launch_name: "grok-4-latest".to_string(),
        model: "grok-4-latest".to_string(),
        subscription: SubscriptionKind::Claude,
        enabled: true,
        free: false,
        official: true,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: EffortMapping::default(),
        quota_lookup_key: None,
        display_order: 0,
    }];

    let (_models, warnings) = assemble_universe(
        dashboard,
        QuotaPayload::default(),
        BTreeMap::new(),
        &all_clis(),
        &providers,
    );

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("provider model 'grok-4-latest' is not present in ipbr"));
    assert!(
        !warnings[0].contains("ipbr-only-row"),
        "unsupported IPBR rows should not warn: {:?}",
        warnings
    );
}

#[test]
fn assemble_collapsed_kimi_selection_uses_ipbr_phase_scores() {
    use crate::selection::config::SelectionPhase;
    use crate::selection::ranking::phase_rank_score;

    let mut entry = make_entry_with_order("kimi-k2.6", "moonshotai", 0);
    entry.score_source = crate::selection::ScoreSource::Ipbr;
    entry.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(82.0),
        review: Some(79.0),
        ..Default::default()
    };

    let dashboard = vec![entry];
    let quotas = make_quota_payload(&[("moonshotai", "kimi-shared", Some(90))]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    let kimi = &models[0];
    assert_eq!(kimi.name, "kimi-k2.6");
    // Build and Review auto-selection must see the ipbr phase scores
    // from the collapsed model.
    assert_eq!(phase_rank_score(kimi, SelectionPhase::Build), Some(82.0));
    assert_eq!(phase_rank_score(kimi, SelectionPhase::Review), Some(79.0));
}

#[test]
fn available_claude_keeps_anthropic_dashboard_entries() {
    let dashboard = vec![make_entry("claude-sonnet-4.6", "anthropic")];
    let quotas = make_quota_payload(&[("claude", "claude-shared", Some(80))]);
    let available = BTreeSet::from([CliKind::Claude]);

    let models =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::Claude);
    assert_eq!(models[0].name, "claude-sonnet-4.6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn stale_on_error_fallback_uses_expired_dashboard() {
    // Fresh (non-expired) dashboard should be used directly without fetching
    let loaded = LoadedCache {
        dashboard: Some(LoadedSection {
            data: vec![make_entry("claude-sonnet-4.6", "claude")],
            expired: false,
        }),
        quotas: Some(LoadedSection {
            data: make_quota_payload(&[("claude", "claude-shared", Some(80))]),
            expired: false,
        }),
        quota_resets: Some(LoadedSection {
            data: make_reset_payload(&[("claude", "claude-sonnet-4.6", None)]),
            expired: false,
        }),
    };

    let models = assemble_from_cache(loaded);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4.6");
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
    let dashboard = vec![make_entry("claude-sonnet-4.6", "claude")];
    let quotas = make_quota_payload(&[("claude", "claude-shared", Some(80))]);
    let resets = empty_resets_for_quotas(&quotas);

    let (models, _warnings) = assemble_universe(dashboard, quotas, resets, &all_clis(), &[]);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4.6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn quota_strict_lookup_returns_none_when_no_exact_match() {
    // Task 6 retired the per-vendor heuristic that used to cross-fill
    // quotas across Claude models. The candidate's quota_lookup_key
    // (or its launch_name fallback) must hit a real entry — otherwise
    // the row reports an unknown quota.
    let dashboard = vec![make_entry("claude-opus-4.7", "claude")];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4.6", Some(75))]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].quota_percent, None);
}

#[test]
fn dedup_keeps_direct_when_quota_meets_floor() {
    // Direct quota at or above the 21% official floor (spec §
    // "Selection algorithm") wins unconditionally — even when opencode
    // reports more remaining headroom.
    for (direct_quota, opencode_quota) in [
        (Some(21), Some(99)),
        (Some(50), Some(50)),
        (Some(80), Some(99)),
        (Some(80), None),
        (Some(21), None),
    ] {
        let survivor = run_dedup(direct_quota, opencode_quota);
        assert_eq!(survivor.subscription, SubscriptionKind::Claude);
        assert_eq!(survivor.name, "claude-opus-4.7");
        assert_eq!(survivor.quota_percent, direct_quota);
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
        assert_eq!(survivor.subscription, SubscriptionKind::Claude);
        assert_eq!(survivor.name, "claude-opus-4.7");
        assert_eq!(survivor.quota_percent, direct_quota);
    }
}

fn run_dedup(direct_quota: Option<u8>, opencode_quota: Option<u8>) -> CachedModel {
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let direct = make_ipbr_entry("claude-opus-4.7", "claude", "claude-opus-4.7");
    let quotas = make_quota_payload(&[
        ("claude", "claude-shared", direct_quota),
        ("opencode-go", "opencode-go/claude-opus-4.7", opencode_quota),
    ]);
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "opencode-go/claude-opus-4.7".to_string(),
        model: "claude-opus-4.7".to_string(),
        subscription: SubscriptionKind::OpencodeGo,
        enabled: true,
        free: false,
        official: false,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: true,
        effort_eligible: false,
        effort_mapping: EffortMapping::default(),
        quota_lookup_key: None,
        display_order: 1,
    }];
    let (models, _warnings) = assemble_universe(
        vec![direct],
        quotas,
        BTreeMap::new(),
        &opencode_available(),
        &providers,
    );
    assert_eq!(models.len(), 1);
    models.into_iter().next().unwrap()
}

#[test]
fn provider_launch_name_can_differ_from_canonical_model_name() {
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let dashboard = make_ipbr_entry("claude-opus-4.7", "anthropic", "claude-opus-4.7");
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "opencode-go/claude-opus-4.7".to_string(),
        model: "claude-opus-4.7".to_string(),
        subscription: SubscriptionKind::OpencodeGo,
        enabled: true,
        free: false,
        official: false,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: false,
        effort_eligible: false,
        effort_mapping: EffortMapping::default(),
        quota_lookup_key: None,
        display_order: 0,
    }];

    let (models, _warnings) = assemble_universe(
        vec![dashboard],
        QuotaPayload::default(),
        BTreeMap::new(),
        &BTreeSet::from([CliKind::Opencode]),
        &providers,
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-opus-4.7");
    assert_eq!(models[0].subscription, SubscriptionKind::OpencodeGo);
    assert_eq!(
        models[0].selected_candidate().unwrap().launch_name,
        "opencode-go/claude-opus-4.7"
    );
    assert_eq!(models[0].quota_percent, None);
}

#[test]
fn merge_preserves_expired_vendor_on_error() {
    // Cached has data for all four vendors.
    let mut cached = QuotaPayload::default();
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
    let mut fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
    fresh.insert(
        SubscriptionKind::Claude,
        BTreeMap::from([("claude-sonnet".to_string(), Some(80))]),
    );

    let merged = merge_quota_payload(&cached, fresh, &BTreeSet::new());

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
    // Cached used the parse_subscription_str alias ("codex") rather than subscription_kind_to_str ("openai").
    let mut cached = QuotaPayload::default();
    cached.insert(
        "codex".to_string(),
        BTreeMap::from([("gpt-5".to_string(), Some(40))]),
    );

    let mut fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
    fresh.insert(
        SubscriptionKind::Codex,
        BTreeMap::from([("gpt-5".to_string(), Some(90))]),
    );

    let merged = merge_quota_payload(&cached, fresh, &BTreeSet::new());

    // The alias entry is dropped (its vendor was refreshed) and the canonical
    // "openai" key carries the fresh value.
    assert!(!merged.contains_key("codex"));
    assert_eq!(
        merged.get("openai").and_then(|m| m.get("gpt-5").copied()),
        Some(Some(90))
    );
}

#[test]
fn merge_records_freshly_failed_subscriptions() {
    // Codex fetch errored this round (no fresh map, present in `failed`)
    // → its prior cached values stay (stale-on-error) AND it gets a
    // failure marker so selection applies the 50% rule.
    let mut cached = QuotaPayload::default();
    cached.insert(
        "openai".to_string(),
        BTreeMap::from([("gpt-5".to_string(), Some(70))]),
    );
    let failed = BTreeSet::from([SubscriptionKind::Codex]);

    let merged = merge_quota_payload(&cached, BTreeMap::new(), &failed);

    assert_eq!(
        merged.get("openai").and_then(|m| m.get("gpt-5").copied()),
        Some(Some(70)),
        "stale-on-error must keep the cached gpt-5 quota"
    );
    assert!(
        merged
            .failed_subscriptions
            .contains(&SubscriptionKind::Codex),
        "failed Codex refresh must be recorded for the 50% assumption"
    );
}

#[test]
fn merge_clears_failure_marker_when_subscription_recovers() {
    let mut cached = QuotaPayload::default();
    cached.failed_subscriptions.insert(SubscriptionKind::Codex);
    let mut fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
    fresh.insert(
        SubscriptionKind::Codex,
        BTreeMap::from([("gpt-5".to_string(), Some(80))]),
    );

    let merged = merge_quota_payload(&cached, fresh, &BTreeSet::new());

    assert!(
        !merged
            .failed_subscriptions
            .contains(&SubscriptionKind::Codex),
        "successful refresh must clear the prior failure marker"
    );
}

#[test]
fn merge_drops_unknown_vendor_keys() {
    let mut cached = QuotaPayload::default();
    cached.insert(
        "aliens".to_string(),
        BTreeMap::from([("ufo-9000".to_string(), Some(33))]),
    );

    let merged = merge_quota_payload(&cached, BTreeMap::new(), &BTreeSet::new());

    assert!(!merged.contains_key("aliens"));
}

#[test]
fn merge_quota_payload_drops_unparseable_subscription() {
    let mut cached = QuotaPayload::default();
    cached
        .values
        .insert("not-a-subscription".to_string(), BTreeMap::new());
    cached.values.insert("claude".to_string(), BTreeMap::new());

    let merged = merge_quota_payload(&cached, BTreeMap::new(), &BTreeSet::new());

    assert!(
        !merged.values.contains_key("not-a-subscription"),
        "unknown subscription key must drop"
    );
    assert!(
        merged.values.contains_key("claude"),
        "tracked subscription preserved"
    );
}

#[test]
fn missing_quota_results_in_none() {
    let dashboard = vec![make_entry("gemini-2.5-pro", "google")];
    let quotas = make_quota_payload(&[]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].quota_percent, None);
}

#[test]
fn reset_coverage_gaps_require_matching_model_keys() {
    let quotas = make_quota_payload(&[
        ("claude", "claude-shared", Some(80)),
        ("claude", "claude-opus-4.1", Some(80)),
    ]);
    let partial_resets = make_reset_payload(&[("claude", "claude-shared", None)]);
    let covered_resets = make_reset_payload(&[
        ("claude", "claude-shared", None),
        ("claude", "claude-opus-4.1", None),
    ]);

    assert!(has_reset_coverage_gaps(&quotas, &partial_resets));
    assert!(!has_reset_coverage_gaps(&quotas, &covered_resets));
}

#[test]
fn dashboard_warnings_are_exposed_as_refresh_diagnostics() {
    let errors = dashboard_warnings_to_quota_errors(vec!["ipbr display_name 'x' collided".into()]);

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].subscription, SubscriptionKind::Claude);
    assert_eq!(
        errors[0].message,
        "dashboard warning: ipbr display_name 'x' collided"
    );
}

fn kimi_opencode_available() -> BTreeSet<CliKind> {
    BTreeSet::from([CliKind::Kimi, CliKind::Opencode])
}

fn make_opencode_kimi_entry(name: &str, match_key: &str) -> DashboardEntry {
    make_ipbr_entry(name, "opencode", match_key)
}

#[test]
#[ignore = "kimi-latest synthesis was retired in Task 6 — strict baked-only is the new contract"]
fn synth_kimi_latest_skipped_when_no_kimi_semver() {
    // Only suffixed kimi variants (no k<major>.<minor>) means no row qualifies
    // as "the latest kimi"; the synth must not fire and opencode keeps the
    // row uncontested.
    let routed = make_opencode_kimi_entry("kimi-k2-thinking", "kimi-k2-thinking");
    let quotas = make_quota_payload(&[
        ("kimi", "kimi-shared", Some(80)),
        ("opencode", "kimi-k2-thinking", Some(40)),
    ]);

    let (models, _warnings) = assemble_universe(
        vec![routed],
        quotas,
        BTreeMap::new(),
        &kimi_opencode_available(),
        &[],
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::OpencodeGo);
    assert_eq!(models[0].name, "kimi-k2-thinking");
}

#[test]
#[ignore = "kimi-latest synthesis was retired in Task 6 — strict baked-only is the new contract"]
fn synth_kimi_latest_skipped_when_kimi_unavailable() {
    // Without Kimi in available_clis the synth must not emit a kimi-vendor
    // row, even when an opencode-routed kimi-2.6 is present.
    let routed = make_opencode_kimi_entry("kimi-k2.6", "kimi-k2-6");
    let quotas = make_quota_payload(&[("opencode", "kimi-k2.6", Some(90))]);
    let available = BTreeSet::from([CliKind::Opencode]);

    let (models, _warnings) =
        assemble_universe(vec![routed], quotas, BTreeMap::new(), &available, &[]);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::OpencodeGo);
    assert!(!models.iter().any(|m| m.name == "kimi-latest"));
}

#[test]
fn ladder_quota_disabled_pool_picked_when_no_free_or_official() {
    // Spec §"Selection algorithm" step 2: when neither F nor O has
    // entries, the no-quota pool (`quota_disabled = true`) wins over
    // the bare non-official pool. Build the candidate list directly
    // — the dashboard pipeline only ever produces `quota_disabled =
    // false` candidates today.
    let mut q_candidate = candidate(
        SubscriptionKind::OpencodeGo,
        CliKind::Opencode,
        "no-quota",
        Some(5),
        0,
    );
    q_candidate.quota_disabled = true;
    let n_candidate = candidate(
        SubscriptionKind::OpencodeGo,
        CliKind::Opencode,
        "non-official",
        Some(99),
        1,
    );
    let candidates = vec![q_candidate, n_candidate];
    let selected = select_candidate_index(&candidates).unwrap();
    assert_eq!(
        selected, 0,
        "quota_disabled (forced 100%) outranks non-official q=99 in the ladder"
    );
}

#[test]
fn ladder_skips_disabled_providers() {
    let mut disabled_free = candidate(
        SubscriptionKind::OpencodeGo,
        CliKind::Opencode,
        "disabled-free",
        Some(100),
        0,
    );
    disabled_free.free = true;
    disabled_free.enabled = false;
    let active_official = candidate(
        SubscriptionKind::Codex,
        CliKind::Codex,
        "gpt-5",
        Some(80),
        1,
    );
    let candidates = vec![disabled_free, active_official];
    let selected = select_candidate_index(&candidates).unwrap();
    assert_eq!(
        selected, 1,
        "disabled free should be skipped so official wins"
    );
}

#[test]
fn ladder_returns_none_when_every_candidate_is_disabled() {
    let mut a = candidate(
        SubscriptionKind::Codex,
        CliKind::Codex,
        "gpt-5",
        Some(80),
        0,
    );
    a.enabled = false;
    let mut b = candidate(
        SubscriptionKind::OpencodeGo,
        CliKind::Opencode,
        "free",
        Some(100),
        1,
    );
    b.free = true;
    b.enabled = false;
    assert_eq!(select_candidate_index(&[a, b]), None);
}

#[test]
fn provider_override_disables_baked_candidate_in_universe() {
    // Operator marks the baked claude/claude-opus-4.7 tuple as
    // disabled via `[[providers]]`; assemble must propagate the flag
    // so `select_candidate_index` skips it. The dashboard row stays
    // present (no auto-deletion), but it has zero selectable
    // candidates — exactly the spec's AC-1 contract for disabled
    // baked tuples.
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let dashboard = vec![make_ipbr_entry(
        "claude-opus-4.7",
        "claude",
        "claude-opus-4.7",
    )];
    let quotas = make_quota_payload(&[("claude", "claude-opus-4.7", Some(80))]);
    let providers = vec![ProviderEntry {
        cli: CliKind::Claude,
        launch_name: "claude-opus-4.7".to_string(),
        model: "claude-opus-4.7".to_string(),
        subscription: SubscriptionKind::Claude,
        enabled: false,
        free: false,
        official: true,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: EffortMapping::default(),
        quota_lookup_key: None,
        display_order: 0,
    }];
    let available = BTreeSet::from([CliKind::Claude]);

    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &available, &providers);

    assert_eq!(models.len(), 1, "row must remain even when disabled");
    let row = &models[0];
    assert_eq!(row.candidates.len(), 1);
    assert!(
        !row.candidates[0].enabled,
        "user override must flip the baked candidate's enabled flag"
    );
    assert!(
        row.selected_candidate().is_none(),
        "ladder must skip disabled candidates"
    );
}

#[test]
fn provider_addition_appends_routed_candidate_to_dashboard_row() {
    // The user adds an opencode-routed alternative for an existing
    // claude-opus-4.7 dashboard row. The natural Claude candidate is
    // still produced (with its baked flags) AND the OpencodeGo
    // candidate appears as an addition with the user-supplied flags.
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let dashboard = vec![make_ipbr_entry(
        "claude-opus-4.7",
        "claude",
        "claude-opus-4.7",
    )];
    let quotas = make_quota_payload(&[
        ("claude", "claude-opus-4.7", Some(50)),
        ("opencode-go", "claude-opus-4.7", Some(95)),
    ]);
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "claude-opus-4.7".to_string(),
        model: "claude-opus-4.7".to_string(),
        subscription: SubscriptionKind::OpencodeGo,
        enabled: true,
        free: false,
        official: false,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: EffortMapping::default(),
        quota_lookup_key: None,
        display_order: 1,
    }];
    let available = BTreeSet::from([CliKind::Claude, CliKind::Opencode]);

    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &available, &providers);

    assert_eq!(models.len(), 1);
    let row = &models[0];
    assert_eq!(row.candidates.len(), 2);
    let opencode = row
        .candidates
        .iter()
        .find(|c| c.subscription == SubscriptionKind::OpencodeGo)
        .expect("user-added opencode candidate must appear in the row");
    assert_eq!(opencode.cli, CliKind::Opencode);
    assert!(opencode.tough_eligible);
    assert!(!opencode.official, "user addition stayed non-official");
}

#[test]
fn assemble_marks_failed_subscription_candidate_with_50_percent_assumption() {
    // Per spec, a subscription that failed its quota fetch has *all*
    // its providers reported as 50% effective when no per-model
    // quota came back. Wire this through assemble_universe and read
    // the candidate's `effective_quota()`. Uses gpt-5-5 because it
    // is in the baked table (the strict-baked path requires it).
    let dashboard = vec![make_ipbr_entry("gpt-5.5", "codex", "gpt-5.5")];
    let mut quotas = QuotaPayload::default();
    quotas.failed_subscriptions.insert(SubscriptionKind::Codex);
    let available = BTreeSet::from([CliKind::Codex]);
    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &available, &[]);
    assert_eq!(models.len(), 1);
    let candidate = &models[0].candidates[0];
    assert!(candidate.quota_failed);
    assert_eq!(candidate.quota_percent, None);
    assert_eq!(candidate.effective_quota(), Some(50));
    // Row-level quota mirrors the selected candidate's effective
    // quota, so downstream UI shows 50% rather than "unknown".
    assert_eq!(models[0].quota_percent, Some(50));
}

#[test]
fn available_clis_filters_by_cli_not_subscription() {
    // A claude-keyed dashboard row produces a Candidate iff the Claude
    // CLI is in `available_clis`. Swapping the available set to
    // {Codex} hides the candidate even though the row's subscription
    // (Claude) is unchanged — confirming that availability now keys on
    // CLI presence rather than subscription presence.
    let dashboard = vec![make_ipbr_entry(
        "claude-opus-4.7",
        "anthropic",
        "claude-opus-4.7",
    )];
    let quotas = make_quota_payload(&[("claude", "claude-opus-4.7", Some(80))]);

    // Claude CLI present → row's natural Claude candidate surfaces.
    let with_claude = BTreeSet::from([CliKind::Claude]);
    let (models, _warnings) = assemble_universe(
        dashboard.clone(),
        quotas.clone(),
        BTreeMap::new(),
        &with_claude,
        &[],
    );
    assert_eq!(models.len(), 1);
    let row = &models[0];
    assert!(
        row.candidates
            .iter()
            .any(|c| c.cli == CliKind::Claude && c.subscription == SubscriptionKind::Claude),
        "Claude CLI must surface the natural Claude candidate"
    );

    // Same dashboard with Codex CLI only → no Claude candidate; the row
    // still exists (model-first invariant) but has zero candidates.
    let with_codex = BTreeSet::from([CliKind::Codex]);
    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &with_codex, &[]);
    assert_eq!(models.len(), 1);
    assert!(
        models[0].candidates.is_empty(),
        "filtering by CLI must hide the Claude candidate when Claude CLI is absent"
    );
}
