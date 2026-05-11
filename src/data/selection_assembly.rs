//! IO-side wrappers around the pure model-universe assembly in
//! `crate::logic::selection::assemble`. Owns every cache read/write,
//! dashboard refresh, and quota refresh involved in producing the
//! `CachedModel` list — the logic layer is forbidden to touch any of
//! these directly (see `scripts/check-layers.sh`).
use crate::cache::{self, LoadedCache};
use crate::dashboard::{self, LoadOutcome};
use crate::data::cache_lock;
use crate::data::config::schema::ProviderEntry;
use crate::data::selection_quota as quota;
use crate::logic::selection::assemble as pure;
use crate::logic::selection::types::{CachedModel, CliKind, QuotaError, SubscriptionKind};
use std::collections::BTreeSet;
use std::path::Path;

pub async fn assemble_models_async(
    cache_dir: &Path,
    available_clis: &BTreeSet<CliKind>,
    providers: &[ProviderEntry],
) -> (Vec<CachedModel>, Vec<QuotaError>) {
    let loaded = cache::load(cache_dir);
    if !refresh_needed(&loaded) {
        return (
            assemble_from_loaded(&loaded, available_clis, providers),
            Vec::new(),
        );
    }
    let lock = cache::lock_path(cache_dir);
    match cache_lock::try_acquire(&lock) {
        Ok(true) => {
            tracing::info!(
                event = "cache_publisher_elected",
                lock_path = %lock.display(),
                "acquired lock for cache refresh"
            );
            let result =
                assemble_with_refresh_unlocked(cache_dir, loaded, available_clis, providers).await;
            if let Err(e) = cache_lock::release(&lock) {
                tracing::warn!(
                    event = "cache_lock_release_failed",
                    lock_path = %lock.display(),
                    error = %e,
                    "failed to release cache lock after refresh"
                );
            }
            return result;
        }
        Ok(false) => tracing::info!(
            event = "cache_follower_skipped_refresh",
            lock_path = %lock.display(),
            "lock held by live PID, rendering cached data"
        ),
        Err(e) => tracing::warn!(
            event = "cache_lock_error",
            lock_path = %lock.display(),
            error = %e,
            "lock acquisition failed, falling back to cached data"
        ),
    }
    (
        assemble_from_loaded(&loaded, available_clis, providers),
        Vec::new(),
    )
}

pub fn assemble_from_cached_only(
    cache_dir: &Path,
    available_clis: &BTreeSet<CliKind>,
    providers: &[ProviderEntry],
) -> Vec<CachedModel> {
    let loaded = cache::load(cache_dir);
    assemble_from_loaded_with_available(&loaded, available_clis, providers)
}

pub fn assemble_from_loaded(
    loaded: &LoadedCache,
    available_clis: &BTreeSet<CliKind>,
    providers: &[ProviderEntry],
) -> Vec<CachedModel> {
    assemble_from_loaded_with_available(loaded, available_clis, providers)
}
fn assemble_from_loaded_with_available(
    loaded: &LoadedCache,
    available_clis: &BTreeSet<CliKind>,
    providers: &[ProviderEntry],
) -> Vec<CachedModel> {
    if loaded.dashboard.is_none() {
        return Vec::new();
    }
    let dashboard = loaded
        .dashboard
        .as_ref()
        .map(|section| section.data.clone())
        .unwrap_or_default();
    let quotas = loaded
        .quotas
        .as_ref()
        .map(|section| section.data.clone())
        .unwrap_or_default();
    let resets = loaded
        .quota_resets
        .as_ref()
        .map(|section| section.data.clone())
        .unwrap_or_default();
    let (models, _free_model_warnings) =
        pure::assemble_universe(dashboard, quotas, resets, available_clis, providers);
    models
}

/// Single source of truth for "does this loaded cache require a network
/// refresh?". Used both at the publisher/follower election boundary in
/// `assemble_models_async` and (via individual fields) inside
/// `assemble_with_refresh_unlocked` to decide which sections to fetch.
/// Returns true when any tracked section is expired or when the quota
/// payload has model entries without matching reset coverage — an
/// absent reset entry is treated as stale (see `has_reset_coverage_gaps`
/// in the pure layer).
fn refresh_needed(loaded: &LoadedCache) -> bool {
    let dashboard_expired = loaded.dashboard.as_ref().map(|s| s.expired).unwrap_or(true);
    let quotas_expired = loaded.quotas.as_ref().map(|s| s.expired).unwrap_or(true);
    let resets_expired = loaded
        .quota_resets
        .as_ref()
        .map(|s| s.expired)
        .unwrap_or(false);
    let reset_missing = match (loaded.quotas.as_ref(), loaded.quota_resets.as_ref()) {
        (Some(q), Some(r)) => pure::has_reset_coverage_gaps(&q.data, &r.data),
        // Missing quotas section is already covered by `quotas_expired`
        // (its default is `true`). Missing resets section while a quotas
        // section exists is itself a coverage gap.
        (Some(_), None) => true,
        _ => false,
    };
    dashboard_expired || quotas_expired || resets_expired || reset_missing
}

async fn assemble_with_refresh_unlocked(
    cache_dir: &Path,
    loaded: LoadedCache,
    available_clis: &BTreeSet<CliKind>,
    providers: &[ProviderEntry],
) -> (Vec<CachedModel>, Vec<QuotaError>) {
    let (cached_dashboard, dashboard_expired) = match loaded.dashboard {
        Some(section) => (section.data, section.expired),
        None => (Vec::new(), true),
    };
    let (cached_quota, quota_expired) = match loaded.quotas {
        Some(section) => (section.data, section.expired),
        None => (crate::cache::QuotaPayload::default(), true),
    };
    let (cached_resets, resets_expired) = match loaded.quota_resets {
        Some(section) => (section.data, section.expired),
        None => (std::collections::BTreeMap::new(), false),
    };
    let mut quota_errors = Vec::new();
    // Dashboard refresh (independent of quota refresh).
    // On error, fall back to expired cached entries (which may be empty).
    let dashboard_entries = if dashboard_expired {
        match dashboard::load_models_async().await {
            Ok(LoadOutcome {
                models: fresh,
                warnings,
            }) => {
                quota_errors.extend(pure::dashboard_warnings_to_quota_errors(warnings));
                let entries = pure::dashboard_models_to_entries(&fresh);
                let _ = cache::save_dashboard_unlocked(cache_dir, &entries);
                entries
            }
            Err(e) => {
                quota_errors.push(QuotaError {
                    subscription: SubscriptionKind::Claude,
                    message: format!("dashboard fetch failed: {e}"),
                });
                cached_dashboard
            }
        }
    } else {
        cached_dashboard
    };
    // Quota refresh (independent of dashboard outcome).
    // On per-vendor error, preserve that vendor's expired cached data so
    // a single failing vendor cannot wipe out previously-known quotas.
    // Old v3 caches can have fresh quotas but no reset section, and newer
    // caches can still have partial gaps after a vendor refresh only wrote
    // some model keys. Treat an absent reset entry as stale, but keep an
    // explicit `None` as the provider's current "no reset time" answer.
    let reset_missing = pure::has_reset_coverage_gaps(&cached_quota, &cached_resets);
    let quota_payload;
    let reset_payload;
    if quota_expired || resets_expired || reset_missing {
        // The fetch set is the intersection of "subscriptions reachable
        // from the launch CLIs" and "subscriptions actually present in
        // the resolved providers list". Direct providers do not appear
        // in this set — `tracked_subscriptions_for_clis` never emits
        // `SubscriptionKind::Direct` — so no API call is made for them.
        let target_subscriptions = quota::fetch_set_for(available_clis.iter().copied(), providers);
        let (fresh_quotas, fresh_resets, fresh_errors) =
            quota::load_quota_maps_for_async(target_subscriptions.iter().copied()).await;
        // Capture the failed vendor set BEFORE consuming `fresh_errors`
        // so the spec's 50% capacity assumption (per QuotaPayload.
        // failed_subscriptions) can be applied even when a vendor's
        // refresh exploded with no per-model entries to insert.
        let failed_vendors: BTreeSet<SubscriptionKind> =
            fresh_errors.iter().map(|err| err.subscription).collect();
        quota_errors.extend(fresh_errors);
        quota_payload = pure::merge_quota_payload(&cached_quota, fresh_quotas, &failed_vendors);
        reset_payload = pure::merge_reset_payload(&cached_resets, fresh_resets);
        let _ = cache::save_quotas_unlocked(cache_dir, &quota_payload);
        let _ = cache::save_quota_resets_unlocked(cache_dir, &reset_payload);
    } else {
        quota_payload = cached_quota;
        reset_payload = cached_resets;
    }
    let (models, _free_model_warnings) = pure::assemble_universe(
        dashboard_entries,
        quota_payload,
        reset_payload,
        available_clis,
        providers,
    );
    (models, quota_errors)
}
#[cfg(test)]
mod tests_mod;
