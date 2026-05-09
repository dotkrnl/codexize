use super::*;
use crate::data::config::schema::{EffortMapping, ProviderEntry};
use crate::logic::selection::types::{CliKind, SubscriptionKind};

fn provider_entry(
    cli: CliKind,
    launch_name: &str,
    subscription: SubscriptionKind,
) -> ProviderEntry {
    ProviderEntry {
        cli,
        launch_name: launch_name.to_string(),
        model: format!("{launch_name}-model"),
        subscription,
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
    }
}

#[tokio::test]
async fn load_quota_maps_for_empty_vendor_set_skips_all_probes() {
    let (maps, reset_maps, errors) = load_quota_maps_for_async([]).await;

    assert!(maps.is_empty());
    assert!(reset_maps.is_empty());
    assert!(errors.is_empty());
}

#[tokio::test]
async fn load_quota_maps_for_direct_only_skips_all_probes() {
    // `Direct` is filtered up front by `load_quota_maps_for_async` so a
    // direct-only fetch set fans out zero worker tasks and surfaces
    // empty maps with no errors.
    let (maps, reset_maps, errors) = load_quota_maps_for_async([SubscriptionKind::Direct]).await;

    assert!(maps.is_empty());
    assert!(reset_maps.is_empty());
    assert!(errors.is_empty());
}

#[test]
fn tracked_subscriptions_for_clis_maps_each_cli_to_its_pool() {
    let mapped = tracked_subscriptions_for_clis([
        CliKind::Claude,
        CliKind::Codex,
        CliKind::Gemini,
        CliKind::Kimi,
        CliKind::Opencode,
    ]);

    assert_eq!(
        mapped,
        BTreeSet::from([
            SubscriptionKind::Claude,
            SubscriptionKind::Codex,
            SubscriptionKind::Gemini,
            SubscriptionKind::Kimi,
            SubscriptionKind::OpencodeGo,
        ])
    );
}

#[test]
fn fetch_set_for_skips_direct_providers() {
    // Mixed universe: a Direct provider sitting on the Codex CLI plus a
    // tracked Claude provider on the Claude CLI. The fetch set must
    // surface only Claude — Direct providers do not back any
    // subscription pool the quota fetcher knows how to probe.
    let providers = vec![
        provider_entry(CliKind::Codex, "openrouter", SubscriptionKind::Direct),
        provider_entry(CliKind::Claude, "claude", SubscriptionKind::Claude),
    ];
    let fetch = fetch_set_for([CliKind::Claude, CliKind::Codex], &providers);

    assert_eq!(
        fetch,
        BTreeSet::from([SubscriptionKind::Claude]),
        "Direct providers must not contribute a probe target"
    );
}

#[test]
fn fetch_set_for_intersects_clis_and_provider_subscriptions() {
    // The CLI set says Claude + Codex are reachable; the provider list
    // only includes a Claude entry. The fetch set must restrict to
    // Claude alone — fanning out a Codex probe would be wasted IO when
    // no Codex provider is configured.
    let providers = vec![provider_entry(
        CliKind::Claude,
        "claude",
        SubscriptionKind::Claude,
    )];
    let fetch = fetch_set_for([CliKind::Claude, CliKind::Codex], &providers);

    assert_eq!(fetch, BTreeSet::from([SubscriptionKind::Claude]));
}

#[test]
fn fetch_set_for_returns_full_cli_set_when_provider_list_empty() {
    // Bootstrap path: the providers list can be empty during early
    // assembly, in which case the fetch set falls back to the full
    // CLI-derived tracked subscription set so cached quotas refresh.
    let fetch = fetch_set_for([CliKind::Claude, CliKind::Opencode], &[]);

    assert_eq!(
        fetch,
        BTreeSet::from([SubscriptionKind::Claude, SubscriptionKind::OpencodeGo])
    );
}

#[test]
fn kimi_quota_takes_min_across_windows() {
    let mapped = live_map_kimi(vec![
        LiveModel {
            name: "kimi-k1.6".to_string(),
            quota_percent: Some(80),
            quota_resets_at: None,
        },
        LiveModel {
            name: "kimi-k2".to_string(),
            quota_percent: Some(20),
            quota_resets_at: None,
        },
    ]);

    let key = providers::kimi::SHARED_QUOTA_KEY;
    assert_eq!(
        mapped.0.get(key),
        Some(&Some(20)),
        "should use the minimum quota across all windows"
    );
    assert_eq!(mapped.1.get(key), Some(&None));
}

#[test]
fn kimi_quota_returns_none_when_all_missing() {
    let mapped = live_map_kimi(vec![LiveModel {
        name: "kimi-k1.6".to_string(),
        quota_percent: None,
        quota_resets_at: None,
    }]);

    assert_eq!(
        mapped.0.get(providers::kimi::SHARED_QUOTA_KEY),
        Some(&None),
        "should return None when no quotas are available"
    );
}

#[test]
fn live_map_normalizes_dotted_canonical_to_dashed_keys() {
    // Live providers return dotted canonical names (e.g. claude-opus-4.7,
    // gemini-2.5-pro) while baked launch_names are dashed
    // (claude-opus-4-7, gemini-2-5-pro). The live-map builders must
    // normalize keys via `normalize_ipbr_key` so the assemble-side lookup
    // by launch_name finds the entry.
    let models = vec![
        LiveModel {
            name: "claude-opus-4.7".to_string(),
            quota_percent: Some(80),
            quota_resets_at: None,
        },
        LiveModel {
            name: "gemini-2.5-pro".to_string(),
            quota_percent: Some(50),
            quota_resets_at: None,
        },
    ];

    let (claude_map, _) = live_map_claude(models.clone());
    assert_eq!(
        claude_map.get("claude-opus-4-7"),
        Some(&Some(80)),
        "dotted canonical must normalize to dashed lookup key"
    );

    let (gemini_map, _) = live_map_direct(models);
    assert_eq!(gemini_map.get("gemini-2-5-pro"), Some(&Some(50)));
}

#[test]
fn live_map_direct_passthrough_per_model() {
    // After dropping the GEMINI_KNOWN_QUOTA_MODELS injection, the map only
    // contains entries for the models the provider's live API actually
    // returned — no synthetic backfill names.
    let mapped = live_map_direct(vec![LiveModel {
        name: "gemini-3-pro".to_string(),
        quota_percent: Some(42),
        quota_resets_at: None,
    }]);

    assert_eq!(mapped.0.len(), 1);
    assert_eq!(mapped.0.get("gemini-3-pro"), Some(&Some(42)));
    assert_eq!(mapped.1.get("gemini-3-pro"), Some(&None));
}
