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
        // Mirror the production fallback in `build_candidate`: anything
        // routed through OpencodeGo is non-official.
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
    let mut direct = make_ipbr_entry("claude-opus-4-7", "claude", "claude-opus-4-7");
    direct.display_order = 0;
    let mut routed = make_ipbr_entry("claude-opus-4-7", "opencode", "claude-opus-4-7");
    routed.display_order = 1;
    let dashboard = vec![direct, routed];
    let quotas = make_quota_payload(&[
        ("claude", "claude-opus-4-7", Some(70)),
        ("opencode-go", "claude-opus-4-7", Some(95)),
    ]);
    // The baked table only carries the Claude provider for
    // claude-opus-4-7; the operator's `[[providers]]` list adds the
    // opencode-routed alternative so both candidates land on the row.
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "claude-opus-4-7".to_string(),
        model: "claude-opus-4-7".to_string(),
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
    assert_eq!(row.name, "claude-opus-4-7");
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

#[test]
#[ignore = "legacy kimi-latest synthesis is retired; baked table maps the kimi row directly"]
fn assemble_universe_collapses_kimi_latest_into_canonical_row() {
    let dashboard = vec![
        make_ipbr_entry("kimi-k2.6", "moonshotai", "kimi-k2.6"),
        make_ipbr_entry("kimi-k2.6", "opencode", "kimi-k2.6"),
    ];
    let quotas = make_quota_payload(&[
        ("moonshotai", "kimi-latest", Some(80)),
        ("opencode-go", "kimi-k2.6", Some(70)),
    ]);
    let available = BTreeSet::from([CliKind::Kimi, CliKind::Opencode]);

    let (models, _warnings) =
        assemble_universe(dashboard, quotas, BTreeMap::new(), &available, &[]);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "kimi-k2.6");
    assert!(
        models[0]
            .candidates
            .iter()
            .any(|candidate| candidate.subscription == SubscriptionKind::Kimi)
    );
    assert!(!models.iter().any(|model| model.name == "kimi-latest"));
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
        dashboard_vendor: vendor.to_string(),
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
        display_order,
        fallback_from: None,
    }
}

fn make_ipbr_entry(name: &str, vendor: &str, match_key: &str) -> DashboardEntry {
    let mut entry = make_entry(name, vendor, 80.0, 78.0);
    entry.score_source = ScoreSource::Ipbr;
    entry.ipbr_row_matched = true;
    entry.ipbr_match_key = Some(match_key.to_string());
    entry
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
    let mut claude_entry = make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0);
    claude_entry
        .axis_provenance
        .insert("correctness".to_string(), "suite:hourly".to_string());
    let dashboard = vec![claude_entry, make_entry("gpt-5-5", "openai", 80.0, 78.0)];
    let quotas = make_quota_payload(&[
        ("claude", "claude-sonnet-4-6", Some(80)),
        ("openai", "gpt-5-5", Some(70)),
    ]);

    let models = assemble_from_cache(loaded_cache_with(dashboard, quotas));

    assert_eq!(models.len(), 2);
    let claude = models
        .iter()
        .find(|m| m.name == "claude-sonnet-4-6")
        .unwrap();
    assert_eq!(claude.subscription, SubscriptionKind::Claude);
    assert_eq!(claude.quota_percent, Some(80));
    assert_eq!(
        claude
            .axis_provenance
            .get("correctness")
            .map(String::as_str),
        Some("suite:hourly")
    );
    let codex = models.iter().find(|m| m.name == "gpt-5-5").unwrap();
    assert_eq!(codex.subscription, SubscriptionKind::Codex);
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
#[ignore = "legacy provider-first Kimi collapse is retired by model-first rows"]
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
    assert_eq!(models[0].subscription, SubscriptionKind::Kimi);
    // Retains the lower-display-order model's cosmetic score (70.0),
    // proving overall_score did not drive the collapse.
    assert_eq!(models[0].overall_score, 70.0);
}

#[test]
#[ignore = "legacy provider-first Kimi collapse is retired by model-first rows"]
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
#[ignore = "legacy provider-first Kimi collapse is retired by model-first rows"]
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
#[ignore = "legacy sibling synthesis is retired by model-first rows"]
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
#[ignore = "legacy synthesized inventory rows are retired by model-first rows"]
fn unavailable_clis_are_omitted_before_models_are_returned() {
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
    let available = BTreeSet::from([CliKind::Codex]);

    let models =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::Codex);
    assert_eq!(models[0].name, "gpt-5.5");
    assert_eq!(models[0].quota_percent, Some(70));
}

#[test]
fn available_claude_keeps_anthropic_dashboard_entries() {
    let dashboard = vec![make_entry("claude-sonnet-4-6", "anthropic", 85.0, 82.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
    let available = BTreeSet::from([CliKind::Claude]);

    let models =
        assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::Claude);
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

    let (models, _warnings) = assemble_universe(dashboard, quotas, resets, &all_clis(), &[]);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-sonnet-4-6");
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn quota_strict_lookup_returns_none_when_no_exact_match() {
    // Task 6 retired the per-vendor heuristic that used to cross-fill
    // quotas across Claude models. The candidate's quota_lookup_key
    // (or its launch_name fallback) must hit a real entry — otherwise
    // the row reports an unknown quota.
    let dashboard = vec![make_entry("claude-opus-4-7", "claude", 90.0, 88.0)];
    let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(75))]);

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
        assert_eq!(survivor.name, "claude-opus-4-7");
        assert_eq!(survivor.quota_percent, direct_quota);
    }
}

#[test]
#[ignore = "legacy routed-model dedup is retired by candidate arbitration"]
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
        assert_eq!(survivor.subscription, SubscriptionKind::OpencodeGo);
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
        assert_eq!(survivor.subscription, SubscriptionKind::Claude);
        assert_eq!(survivor.name, "claude-opus-4-7");
        assert_eq!(survivor.quota_percent, direct_quota);
    }
}

fn run_dedup(direct_quota: Option<u8>, opencode_quota: Option<u8>) -> CachedModel {
    let direct = make_ipbr_entry("claude-opus-4-7", "claude", "claude-opus-4-7");
    let mut routed = direct.clone();
    routed.name = "opencode/claude-opus-4-7".to_string();
    routed.dashboard_vendor = "opencode".to_string();
    let quotas = make_quota_payload(&[
        ("claude", "claude-opus-4-7", direct_quota),
        ("opencode", "opencode/claude-opus-4-7", opencode_quota),
    ]);
    let (models, _warnings) = assemble_universe(
        vec![direct, routed],
        quotas,
        BTreeMap::new(),
        &opencode_available(),
        &[],
    );
    assert_eq!(models.len(), 1);
    models.into_iter().next().unwrap()
}

#[test]
fn opencode_ipbr_matched_inventory_renders_with_unknown_quota() {
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let routed = make_ipbr_entry("opencode/claude-opus-4-7", "opencode", "claude-opus-4-7");
    // The unbaked dashboard row needs an explicit provider entry now
    // that synthesis is gone; the operator-supplied route is what
    // turns into the candidate.
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "opencode/claude-opus-4-7".to_string(),
        model: "opencode/claude-opus-4-7".to_string(),
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
        vec![routed],
        QuotaPayload::default(),
        BTreeMap::new(),
        &opencode_available(),
        &providers,
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::OpencodeGo);
    assert_eq!(models[0].quota_percent, None);
    assert_eq!(models[0].ipbr_match_key.as_deref(), Some("claude-opus-4-7"));
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
    // After Task 6 the merge is strict: unparseable subscription keys
    // (e.g. legacy "free" rows from a previous schema, or this
    // "aliens" sentinel) are dropped on the next refresh round so the
    // cache cannot accumulate stale, untracked entries forever.
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
    // A legacy cache that still has a "free" row (the schema dropped
    // SubscriptionKind::Free in Task 1) must lose that row on the
    // first refresh; tracked subscription strings are preserved.
    let mut cached = QuotaPayload::default();
    cached.values.insert("free".to_string(), BTreeMap::new());
    cached.values.insert("claude".to_string(), BTreeMap::new());

    let merged = merge_quota_payload(&cached, BTreeMap::new(), &BTreeSet::new());

    assert!(
        !merged.values.contains_key("free"),
        "stale 'free' key must drop"
    );
    assert!(
        merged.values.contains_key("claude"),
        "tracked subscription preserved"
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
    assert_eq!(errors[0].vendor, SubscriptionKind::Claude);
    assert_eq!(
        errors[0].message,
        "dashboard warning: ipbr normalized key 'x' collided"
    );
}

fn kimi_opencode_available() -> BTreeSet<CliKind> {
    BTreeSet::from([CliKind::Kimi, CliKind::Opencode])
}

fn make_opencode_kimi_entry(name: &str, match_key: &str) -> DashboardEntry {
    make_ipbr_entry(name, "opencode", match_key)
}

#[test]
#[ignore = "legacy kimi-latest synthesis is retired by canonical rows"]
fn synth_kimi_latest_wins_when_kimi_quota_meets_floor() {
    // kimi-code's shared pool is at 50% — well above the 20% floor — so the
    // synthesized direct route must beat the opencode-routed sibling for the
    // same kimi-2.6 ipbr row.
    let routed = make_opencode_kimi_entry("kimi-k2.6", "kimi-k2-6");
    let quotas = make_quota_payload(&[
        ("kimi", "kimi-shared", Some(50)),
        ("opencode", "kimi-k2.6", Some(90)),
    ]);

    let (models, _warnings) = assemble_universe(
        vec![routed],
        quotas,
        BTreeMap::new(),
        &kimi_opencode_available(),
        &[],
    );

    assert_eq!(models.len(), 1);
    let survivor = &models[0];
    assert_eq!(survivor.subscription, SubscriptionKind::Kimi);
    assert_eq!(survivor.name, "kimi-latest");
    assert_eq!(survivor.quota_percent, Some(50));
    assert_eq!(survivor.ipbr_match_key.as_deref(), Some("kimi-k2-6"));
}

#[test]
#[ignore = "legacy kimi-latest synthesis is retired by canonical rows"]
fn synth_kimi_latest_loses_to_opencode_when_kimi_below_floor_and_opencode_higher() {
    // kimi-code below the 20% floor + opencode strictly higher → dedup defers
    // to opencode for this kimi row.
    let routed = make_opencode_kimi_entry("kimi-k2.6", "kimi-k2-6");
    let quotas = make_quota_payload(&[
        ("kimi", "kimi-shared", Some(5)),
        ("opencode", "kimi-k2.6", Some(80)),
    ]);

    let (models, _warnings) = assemble_universe(
        vec![routed],
        quotas,
        BTreeMap::new(),
        &kimi_opencode_available(),
        &[],
    );

    assert_eq!(models.len(), 1);
    let survivor = &models[0];
    assert_eq!(survivor.subscription, SubscriptionKind::OpencodeGo);
    assert_eq!(survivor.name, "kimi-k2.6");
    assert_eq!(survivor.quota_percent, Some(80));
}

#[test]
#[ignore = "legacy kimi-latest synthesis is retired by canonical rows"]
fn synth_kimi_latest_wins_when_opencode_quota_unknown() {
    // Direct quota unknown vs opencode unknown → direct wins per the
    // existing dedup arm. The synth must still be present so this branch is
    // reachable for kimi.
    let routed = make_opencode_kimi_entry("kimi-k2.6", "kimi-k2-6");
    let quotas = make_quota_payload(&[]);

    let (models, _warnings) = assemble_universe(
        vec![routed],
        quotas,
        BTreeMap::new(),
        &kimi_opencode_available(),
        &[],
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].subscription, SubscriptionKind::Kimi);
    assert_eq!(models[0].name, "kimi-latest");
    assert_eq!(models[0].quota_percent, None);
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
#[ignore = "legacy kimi-latest synthesis is retired by canonical rows"]
fn synth_kimi_latest_picks_highest_semver_among_routes() {
    // With multiple opencode-routed kimis the synth must mirror the highest
    // semver — kimi-k2.6 here, not kimi-k1.5 — so the dedup pairing lands
    // against the right opencode sibling.
    let older = make_opencode_kimi_entry("kimi-k1.5", "kimi-k1-5");
    let latest = make_opencode_kimi_entry("kimi-k2.6", "kimi-k2-6");
    let quotas = make_quota_payload(&[
        ("kimi", "kimi-shared", Some(80)),
        ("opencode", "kimi-k1.5", Some(70)),
        ("opencode", "kimi-k2.6", Some(70)),
    ]);

    let (models, _warnings) = assemble_universe(
        vec![older, latest],
        quotas,
        BTreeMap::new(),
        &kimi_opencode_available(),
        &[],
    );

    // Surviving rows: kimi-latest (synth, won dedup over kimi-k2.6) and
    // opencode kimi-k1.5 (different ipbr_match_key, no dedup pair).
    let kimi_latest = models
        .iter()
        .find(|m| m.name == "kimi-latest")
        .expect("synth kimi-latest should survive dedup with kimi above floor");
    assert_eq!(kimi_latest.subscription, SubscriptionKind::Kimi);
    assert_eq!(kimi_latest.ipbr_match_key.as_deref(), Some("kimi-k2-6"));
    assert!(
        models.iter().any(|m| m.name == "kimi-k1.5"),
        "older opencode kimi without a direct sibling stays in the universe"
    );
    assert!(
        !models.iter().any(|m| m.name == "kimi-k2.6"),
        "kimi-k2.6 opencode entry must be deduped out by the synth"
    );
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
    // Operator marks the baked claude/claude-opus-4-7 tuple as
    // disabled via `[[providers]]`; assemble must propagate the flag
    // so `select_candidate_index` skips it. The dashboard row stays
    // present (no auto-deletion), but it has zero selectable
    // candidates — exactly the spec's AC-1 contract for disabled
    // baked tuples.
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let dashboard = vec![make_ipbr_entry(
        "claude-opus-4-7",
        "claude",
        "claude-opus-4-7",
    )];
    let quotas = make_quota_payload(&[("claude", "claude-opus-4-7", Some(80))]);
    let providers = vec![ProviderEntry {
        cli: CliKind::Claude,
        launch_name: "claude-opus-4-7".to_string(),
        model: "claude-opus-4-7".to_string(),
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
    // claude-opus-4-7 dashboard row. The natural Claude candidate is
    // still produced (with its baked flags) AND the OpencodeGo
    // candidate appears as an addition with the user-supplied flags.
    use crate::data::config::schema::{EffortMapping, ProviderEntry};
    let dashboard = vec![make_ipbr_entry(
        "claude-opus-4-7",
        "claude",
        "claude-opus-4-7",
    )];
    let quotas = make_quota_payload(&[
        ("claude", "claude-opus-4-7", Some(50)),
        ("opencode-go", "claude-opus-4-7", Some(95)),
    ]);
    let providers = vec![ProviderEntry {
        cli: CliKind::Opencode,
        launch_name: "claude-opus-4-7".to_string(),
        model: "claude-opus-4-7".to_string(),
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
    let dashboard = vec![make_ipbr_entry("gpt-5-5", "codex", "gpt-5-5")];
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
        "claude-opus-4-7",
        "anthropic",
        "claude-opus-4-7",
    )];
    let quotas = make_quota_payload(&[("claude", "claude-opus-4-7", Some(80))]);

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
