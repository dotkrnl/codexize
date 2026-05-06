//! IO-side wrappers around the pure model-universe assembly in
//! `crate::logic::selection::assemble`. Owns every cache read/write,
//! dashboard refresh, and quota refresh involved in producing the
//! `CachedModel` list — the logic layer is forbidden to touch any of
//! these directly (see `scripts/check-layers.sh`).

use crate::acp::AcpConfig;
use crate::cache::{self, LoadedCache};
use crate::dashboard::{self, LoadOutcome};
use crate::data::selection_quota as quota;
use crate::logic::selection::assemble as pure;
use crate::logic::selection::types::{CachedModel, QuotaError, VendorKind};
use std::collections::BTreeSet;

pub async fn assemble_models_async() -> (Vec<CachedModel>, Vec<QuotaError>) {
    let loaded = cache::load();
    assemble_with_refresh(loaded, &AcpConfig::default().available_vendors()).await
}

/// Build the canonical model universe purely from cached data, performing no
/// network fetches. Returns an empty vector if the dashboard cache is missing.
/// Useful at startup to render the model strip immediately while a background
/// refresh runs.
pub fn assemble_from_cached_only() -> Vec<CachedModel> {
    let loaded = cache::load();
    assemble_from_loaded(&loaded)
}

/// Build the canonical model universe from an already-loaded cache snapshot.
///
/// Does not consult the network; treats every section as authoritative. The
/// available-vendor probe still runs because vendor availability is the
/// caller-policy gate, not part of the cache snapshot.
pub fn assemble_from_loaded(loaded: &LoadedCache) -> Vec<CachedModel> {
    assemble_from_loaded_with_available(loaded, &AcpConfig::default().available_vendors())
}

fn assemble_from_loaded_with_available(
    loaded: &LoadedCache,
    available_vendors: &BTreeSet<VendorKind>,
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
    pure::assemble_universe(dashboard, quotas, resets, available_vendors)
}

async fn assemble_with_refresh(
    loaded: LoadedCache,
    available_vendors: &BTreeSet<VendorKind>,
) -> (Vec<CachedModel>, Vec<QuotaError>) {
    let (cached_dashboard, dashboard_expired) = match loaded.dashboard {
        Some(section) => (section.data, section.expired),
        None => (Vec::new(), true),
    };
    let (cached_quota, quota_expired) = match loaded.quotas {
        Some(section) => (section.data, section.expired),
        None => (std::collections::BTreeMap::new(), true),
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
            Ok(LoadOutcome::Both {
                models: fresh,
                warnings,
            }) => {
                quota_errors.extend(pure::dashboard_warnings_to_quota_errors(warnings));
                let entries = pure::dashboard_models_to_entries(&fresh);
                let _ = cache::save_dashboard(&entries);
                entries
            }
            Ok(LoadOutcome::InventoryOnly {
                models,
                score_error,
            }) => {
                quota_errors.push(QuotaError {
                    vendor: VendorKind::Claude,
                    message: format!("dashboard fetch failed: {score_error}"),
                });
                pure::resolve_score_failure_entries(
                    cached_dashboard,
                    pure::dashboard_models_to_entries(&models),
                )
            }
            Err(e) => {
                quota_errors.push(QuotaError {
                    vendor: VendorKind::Claude,
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
        let (fresh_quotas, fresh_resets, fresh_errors) =
            quota::load_quota_maps_for_async(available_vendors.iter().copied()).await;
        quota_errors.extend(fresh_errors);
        quota_payload = pure::merge_quota_payload(&cached_quota, fresh_quotas);
        reset_payload = pure::merge_reset_payload(&cached_resets, fresh_resets);
        let _ = cache::save_quotas(&quota_payload);
        let _ = cache::save_quota_resets(&reset_payload);
    } else {
        quota_payload = cached_quota;
        reset_payload = cached_resets;
    }

    let models = pure::assemble_universe(
        dashboard_entries,
        quota_payload,
        reset_payload,
        available_vendors,
    );
    (models, quota_errors)
}

#[cfg(test)]
mod tests_mod;
