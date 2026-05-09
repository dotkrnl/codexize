//! Backend probes that resolve per-vendor quota and reset maps from the
//! provider adapters.
use crate::data::config::schema::ProviderEntry;
use crate::data::providers::{self, LiveModel};
use crate::logic::selection::types::{CliKind, QuotaError, SubscriptionKind};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};
type VendorQuotaMap = BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>;
type VendorResetMap = BTreeMap<SubscriptionKind, BTreeMap<String, Option<DateTime<Utc>>>>;
type ModelQuotaMap = BTreeMap<String, Option<u8>>;
type ModelResetMap = BTreeMap<String, Option<DateTime<Utc>>>;
type ModelQuotaAndResetMaps = (ModelQuotaMap, ModelResetMap);
type QuotaLoadResult = (VendorQuotaMap, VendorResetMap, Vec<QuotaError>);
/// Quota probes are subscription-keyed, but the launch boundary now exposes
/// only `CliKind`. Cross the boundary HERE — keep the mapper next to the
/// consumer that cares about it — and filter to the five tracked
/// subscriptions in the same step. `SubscriptionKind::Direct` is implicitly
/// excluded because no `CliKind` value maps to it: direct-billed providers
/// route through whichever real CLI the operator configured, not a "Direct"
/// CLI of their own. The provider-set caller still needs to be honest about
/// which CLIs actually have a tracked subscription pool to query — that
/// honesty is what this function enforces.
pub fn tracked_subscriptions_for_clis<I: IntoIterator<Item = CliKind>>(
    clis: I,
) -> BTreeSet<SubscriptionKind> {
    clis.into_iter()
        .map(|cli| match cli {
            CliKind::Claude => SubscriptionKind::Claude,
            CliKind::Codex => SubscriptionKind::Codex,
            CliKind::Gemini => SubscriptionKind::Gemini,
            CliKind::Kimi => SubscriptionKind::Kimi,
            CliKind::Opencode => SubscriptionKind::OpencodeGo,
        })
        .collect()
}
/// Compute the set of subscriptions actually worth probing for the given
/// `available_clis` and resolved `providers`. The caller passes both the
/// CLI set (which subscriptions could be queried at all) and the provider
/// list (which subscriptions the operator's universe currently includes).
/// The intersection is the fetch set; `SubscriptionKind::Direct` rows in
/// `providers` are skipped automatically because `tracked_subscriptions_for_clis`
/// never emits `Direct`.
pub fn fetch_set_for(
    available_clis: impl IntoIterator<Item = CliKind>,
    providers: &[ProviderEntry],
) -> BTreeSet<SubscriptionKind> {
    let from_clis = tracked_subscriptions_for_clis(available_clis);
    if providers.is_empty() {
        return from_clis;
    }
    let from_providers: BTreeSet<SubscriptionKind> = providers
        .iter()
        .map(|p| p.subscription)
        .filter(|s| {
            matches!(
                s,
                SubscriptionKind::Claude
                    | SubscriptionKind::Codex
                    | SubscriptionKind::Gemini
                    | SubscriptionKind::Kimi
                    | SubscriptionKind::OpencodeGo
            )
        })
        .collect();
    from_clis.intersection(&from_providers).copied().collect()
}
pub async fn load_quota_maps_for_async(
    subscriptions: impl IntoIterator<Item = SubscriptionKind>,
) -> QuotaLoadResult {
    // Defense-in-depth: the public API still accepts a `SubscriptionKind`
    // iterator for direct test use, but `Direct` is never a meaningful
    // probe target — `load_quota_map_for_subscription` would just return
    // empty maps for it. Drop it up front so callers cannot accidentally
    // fan out a no-op task into the worker pool.
    let subscriptions = subscriptions
        .into_iter()
        .filter(|s| !matches!(s, SubscriptionKind::Direct))
        .collect::<Vec<_>>();
    let tasks = subscriptions
        .into_iter()
        .map(|subscription| {
            (
                subscription,
                tokio::spawn(async move { load_quota_map_for_subscription(subscription).await }),
            )
        })
        .collect::<Vec<_>>();
    let mut maps = BTreeMap::new();
    let mut reset_maps = BTreeMap::new();
    let mut errors = Vec::new();
    for (subscription, task) in tasks {
        let Ok(result) = task.await else {
            errors.push(QuotaError {
                subscription,
                message: "quota worker task failed".to_string(),
            });
            continue;
        };
        match result {
            Ok((map, reset_map)) => {
                maps.insert(subscription, map);
                reset_maps.insert(subscription, reset_map);
            }
            Err(e) => errors.push(QuotaError {
                subscription,
                message: e,
            }),
        }
    }
    (maps, reset_maps, errors)
}
async fn load_quota_map_for_subscription(
    subscription: SubscriptionKind,
) -> Result<ModelQuotaAndResetMaps, String> {
    match subscription {
        SubscriptionKind::Codex => providers::codex::load_live_models_async()
            .await
            .map(live_map_codex)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Claude => providers::claude::load_live_models_async()
            .await
            .map(live_map_claude)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Gemini => providers::gemini::load_live_models_async()
            .await
            .map(live_map_direct)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Kimi => providers::kimi::load_live_models_async()
            .await
            .map(live_map_kimi)
            .map_err(|e| e.to_string()),
        SubscriptionKind::OpencodeGo => providers::opencode::load_live_models_async()
            .await
            .map(live_map_opencode)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Direct => Ok((BTreeMap::new(), BTreeMap::new())),
    }
}
fn live_map_codex(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let mapped: BTreeMap<String, Option<u8>> = models
        .into_iter()
        .map(|m| (m.name.to_ascii_lowercase(), m.quota_percent))
        .collect();
    let resets = mapped.keys().map(|name| (name.clone(), None)).collect();
    (mapped, resets)
}
fn live_map_claude(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let mapped: BTreeMap<String, Option<u8>> = models
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m.quota_percent))
        .collect();
    let resets: BTreeMap<String, Option<DateTime<Utc>>> = models
        .into_iter()
        .map(|m| (m.name.to_ascii_lowercase(), m.quota_resets_at))
        .collect();
    (mapped, resets)
}
fn live_map_direct(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let mapped: BTreeMap<String, Option<u8>> = models
        .into_iter()
        .map(|m| (m.name.to_ascii_lowercase(), m.quota_percent))
        .collect();
    let resets = mapped.keys().map(|name| (name.clone(), None)).collect();
    (mapped, resets)
}
fn live_map_opencode(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    // Opencode runs on a single Go-tier dollar pool, so any non-None entry
    // returned by the provider applies to every opencode-routed model name.
    // Surface a single shared key — baked entries point their
    // `quota_lookup_key` at it so per-row lookups resolve here.
    let quota = models.into_iter().find_map(|m| m.quota_percent);
    (
        BTreeMap::from([(providers::opencode::SHARED_QUOTA_KEY.to_string(), quota)]),
        BTreeMap::from([(providers::opencode::SHARED_QUOTA_KEY.to_string(), None)]),
    )
}
fn live_map_kimi(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    // Kimi runs every model off one shared usage pool, so we expose the
    // quota under a single sentinel key. Baked Kimi entries set
    // `quota_lookup_key = "kimi-shared"` so per-row lookups resolve here
    // without aliasing a real ipbr model id (the way the former
    // `kimi-latest` placeholder did).
    let quota = models.into_iter().filter_map(|m| m.quota_percent).min();
    (
        BTreeMap::from([(providers::kimi::SHARED_QUOTA_KEY.to_string(), quota)]),
        BTreeMap::from([(providers::kimi::SHARED_QUOTA_KEY.to_string(), None)]),
    )
}
#[cfg(test)]
#[path = "selection_quota_tests.rs"]
mod tests;
