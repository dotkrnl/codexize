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
    let tasks = subscriptions
        .into_iter()
        .filter(|s| !matches!(s, SubscriptionKind::Direct))
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
            .map(live_map)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Claude => providers::claude::load_live_models_async()
            .await
            .map(live_map)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Gemini => providers::gemini::load_live_models_async()
            .await
            .map(live_map)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Kimi => providers::kimi::load_live_models_async()
            .await
            .map(live_map)
            .map_err(|e| e.to_string()),
        SubscriptionKind::OpencodeGo => providers::opencode::load_live_models_async()
            .await
            .map(live_map)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Direct => Ok((BTreeMap::new(), BTreeMap::new())),
    }
}
/// Trivial mapper: every provider returns `LiveModel` entries already
/// keyed by the same shape the baked `launch_name` (or
/// `quota_lookup_key`) carries. Shared-pool subscriptions
/// (claude/kimi/opencode) emit a single sentinel `LiveModel`
/// (`claude-shared` / `kimi-shared` / `opencode-shared`); per-model
/// subscriptions (codex/gemini) emit one entry per advertised model,
/// already in the dotted shape the CLI accepts. No per-CLI key
/// transformation is needed here.
fn live_map(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let mapped: BTreeMap<String, Option<u8>> = models
        .iter()
        .map(|m| (m.name.clone(), m.quota_percent))
        .collect();
    let resets: BTreeMap<String, Option<DateTime<Utc>>> = models
        .into_iter()
        .map(|m| (m.name, m.quota_resets_at))
        .collect();
    (mapped, resets)
}
#[cfg(test)]
#[path = "selection_quota_tests.rs"]
mod tests;
