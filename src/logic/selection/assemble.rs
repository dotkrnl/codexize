//! Assembles the canonical model universe by combining the cached dashboard
//! with live quota data, applying Kimi collapse and sibling synthesis before
//! handing the result back to the caller.
//!
//! Lives under `logic/` per the refactor task even though it routes through
//! `crate::cache` / `crate::dashboard` for its inputs: the orchestration here
//! is logic (merge rules, Kimi collapse policy, fallback policy on dashboard
//! refresh failure), and the `cache` / `dashboard` crate-root aliases hide
//! the actual `data/` IO that fulfils those reads.

use super::ranking::stamp_selection_provenance;
use super::types::{CachedModel, QuotaError, ScoreSource, VendorKind};
use super::vendor;
use crate::acp::AcpConfig;
use crate::cache::{self, DashboardEntry, LoadedCache, QuotaPayload, ResetPayload};
use crate::dashboard::{self, LoadOutcome};
use crate::selection::quota;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Load and merge dashboard + quota data into the canonical model universe.
///
/// Returns the assembled models and any quota errors encountered.
/// Performs Kimi collapse and sibling synthesis before returning.
pub fn assemble_models() -> (Vec<CachedModel>, Vec<QuotaError>) {
    let loaded = cache::load();
    assemble_from_cache_with_available(loaded, &AcpConfig::default().available_vendors())
}

/// Build the canonical model universe purely from cached data, performing no
/// network fetches. Returns an empty vector if the dashboard cache is missing.
/// Useful at startup to render the model strip immediately while a background
/// refresh runs.
pub fn assemble_from_cached_only() -> Vec<CachedModel> {
    let loaded = cache::load();
    assemble_from_loaded(&loaded)
}

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
    let quotas = loaded
        .quotas
        .as_ref()
        .map(|section| crate::cache::LoadedSection {
            data: section.data.clone(),
            expired: false,
        });
    let quota_resets = loaded
        .quota_resets
        .as_ref()
        .map(|section| crate::cache::LoadedSection {
            data: section.data.clone(),
            expired: false,
        });
    let dashboard = loaded
        .dashboard
        .as_ref()
        .map(|section| crate::cache::LoadedSection {
            data: section.data.clone(),
            expired: false,
        });
    let (models, _) = assemble_from_cache_with_available(
        LoadedCache {
            dashboard,
            quotas,
            quota_resets,
        },
        available_vendors,
    );
    models
}

#[cfg(test)]
fn assemble_from_cache(loaded: LoadedCache) -> (Vec<CachedModel>, Vec<QuotaError>) {
    let available_vendors = [
        VendorKind::Codex,
        VendorKind::Claude,
        VendorKind::Gemini,
        VendorKind::Kimi,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assemble_from_cache_with_available(loaded, &available_vendors)
}

fn assemble_from_cache_with_available(
    loaded: LoadedCache,
    available_vendors: &BTreeSet<VendorKind>,
) -> (Vec<CachedModel>, Vec<QuotaError>) {
    let (cached_dashboard, dashboard_expired) = match loaded.dashboard {
        Some(section) => (section.data, section.expired),
        None => (Vec::new(), true),
    };

    let (cached_quota, quota_expired) = match loaded.quotas {
        Some(section) => (section.data, section.expired),
        None => (BTreeMap::new(), true),
    };
    let (cached_resets, resets_expired) = match loaded.quota_resets {
        Some(section) => (section.data, section.expired),
        None => (BTreeMap::new(), false),
    };

    let mut quota_errors = Vec::new();

    // Dashboard refresh (independent of quota refresh).
    // On error, fall back to expired cached entries (which may be empty).
    let dashboard_entries = if dashboard_expired {
        match dashboard::load_models() {
            Ok(LoadOutcome::Both {
                models: fresh,
                warnings,
            }) => {
                quota_errors.extend(dashboard_warnings_to_quota_errors(warnings));
                let entries = dashboard_models_to_entries(&fresh);
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
                resolve_score_failure_entries(
                    cached_dashboard,
                    dashboard_models_to_entries(&models),
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
    let reset_missing = has_reset_coverage_gaps(&cached_quota, &cached_resets);
    let quota_payload;
    let reset_payload;
    if quota_expired || resets_expired || reset_missing {
        let (fresh_quotas, fresh_resets, fresh_errors) =
            quota::load_quota_maps_for(available_vendors.iter().copied());
        quota_errors.extend(fresh_errors);
        quota_payload = merge_quota_payload(&cached_quota, fresh_quotas);
        reset_payload = merge_reset_payload(&cached_resets, fresh_resets);
        let _ = cache::save_quotas(&quota_payload);
        let _ = cache::save_quota_resets(&reset_payload);
    } else {
        quota_payload = cached_quota;
        reset_payload = cached_resets;
    }

    // Parse quota payload into typed map.
    // Keys may come from vendor_kind_to_str ("openai", "google", "moonshotai")
    // or from str_to_vendor-compatible strings ("codex", "gemini", "kimi").
    let parsed_quotas: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = quota_payload
        .into_iter()
        .filter_map(|(vendor_name, models)| parse_vendor_str(&vendor_name).map(|v| (v, models)))
        .filter(|(vendor, _)| available_vendors.contains(vendor))
        .collect();
    let parsed_resets: BTreeMap<
        VendorKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    > = reset_payload
        .into_iter()
        .filter_map(|(vendor_name, models)| parse_vendor_str(&vendor_name).map(|v| (v, models)))
        .filter(|(vendor, _)| available_vendors.contains(vendor))
        .collect();

    // Convert dashboard entries to DashboardModels for sibling synthesis
    let mut dashboard_models: Vec<dashboard::DashboardModel> = dashboard_entries
        .into_iter()
        .filter(|entry| {
            parse_vendor_str(&entry.vendor)
                .is_some_and(|vendor| available_vendors.contains(&vendor))
        })
        .map(|e| dashboard::DashboardModel {
            name: e.name,
            vendor: e.vendor,
            overall_score: e.overall_score,
            current_score: e.current_score,
            standard_error: e.standard_error,
            axes: e.axes,
            axis_provenance: e.axis_provenance,
            ipbr_phase_scores: e.ipbr_phase_scores,
            score_source: e.score_source,
            ipbr_row_matched: e.ipbr_row_matched,
            display_order: e.display_order,
            fallback_from: e.fallback_from,
        })
        .collect();

    // Synthesize entries for live-quota models missing from the ranking API
    let existing: HashSet<String> = dashboard_models.iter().map(|m| m.name.clone()).collect();
    let mut synthesized: HashSet<String> = HashSet::new();
    for (vendor_kind, models) in &parsed_quotas {
        let vendor_str = vendor::vendor_kind_to_str(*vendor_kind);
        for name in models.keys() {
            if existing.contains(name) || synthesized.contains(name) {
                continue;
            }
            if let Some(model) = dashboard::synthesize_sibling(name, vendor_str, &dashboard_models)
            {
                synthesized.insert(name.clone());
                dashboard_models.push(model);
            }
        }
    }

    // Build CachedModel list — omit models with no dashboard entry (guaranteed by
    // the fact we only iterate dashboard_models)
    let mut models: Vec<CachedModel> = dashboard_models
        .into_iter()
        .filter_map(|m| {
            let vendor = vendor::vendor_for_dashboard_model(&m)?;
            let quota_percent = parsed_quotas
                .get(&vendor)
                .and_then(|by_model| by_model.get(&m.name))
                .copied()
                .flatten()
                .or_else(|| quota::find_quota_by_heuristic(&m.name, vendor, &parsed_quotas));
            let quota_resets_at = parsed_resets
                .get(&vendor)
                .and_then(|by_model| by_model.get(&m.name))
                .copied()
                .flatten()
                .or_else(|| quota::find_reset_by_heuristic(&m.name, vendor, &parsed_resets));

            Some(CachedModel {
                vendor,
                name: m.name,
                overall_score: m.overall_score,
                current_score: m.current_score,
                standard_error: m.standard_error,
                axes: m.axes,
                axis_provenance: m.axis_provenance,
                ipbr_phase_scores: m.ipbr_phase_scores,
                score_source: m.score_source,
                ipbr_row_matched: m.ipbr_row_matched,
                quota_percent,
                quota_resets_at,
                display_order: m.display_order,
                fallback_from: m.fallback_from,
            })
        })
        .collect();

    // Collapse all Kimi models into a single "kimi-latest" representative.
    // The canonical model is chosen by stable inventory order (lowest
    // display_order first, then name for determinism), NOT by cosmetic
    // overall_score or current_score. This ensures the retained phase
    // scores and quota come from a policy-driven choice rather than a
    // display-only summary that must not affect selection.
    let best_kimi_idx = models
        .iter()
        .enumerate()
        .filter(|(_, m)| m.vendor == VendorKind::Kimi)
        .min_by(|(_, a), (_, b)| {
            a.display_order
                .cmp(&b.display_order)
                .then_with(|| a.name.cmp(&b.name))
        })
        .map(|(i, _)| i);
    if let Some(i) = best_kimi_idx {
        let mut canonical = models[i].clone();
        canonical.name = "kimi-latest".to_string();
        models.retain(|m| m.vendor != VendorKind::Kimi);
        models.push(canonical);
    }

    // Stamp fallback:overall provenance for zero-as-missing and truly-missing
    // axes, and emit selection.zero_as_missing counters.
    for model in &mut models {
        stamp_selection_provenance(model);
    }

    (models, quota_errors)
}

/// Merge a freshly-fetched quota map (keyed by `VendorKind`) into the cached
/// payload (keyed by vendor string). Successfully-refreshed vendors overwrite
/// cached entries; cached entries for vendors that did not refresh
/// successfully are carried forward (stale-on-error fallback).
fn merge_quota_payload(
    cached: &QuotaPayload,
    fresh: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>,
) -> QuotaPayload {
    let succeeded: HashSet<VendorKind> = fresh.keys().copied().collect();
    let mut merged: QuotaPayload = BTreeMap::new();

    for (vendor_str, models) in cached {
        let preserve = match parse_vendor_str(vendor_str) {
            Some(kind) => !succeeded.contains(&kind),
            None => true,
        };
        if preserve {
            merged.insert(vendor_str.clone(), models.clone());
        }
    }
    for (vendor, models) in fresh {
        merged.insert(vendor::vendor_kind_to_str(vendor).to_string(), models);
    }
    merged
}

fn merge_reset_payload(
    cached: &ResetPayload,
    fresh: BTreeMap<VendorKind, BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>>,
) -> ResetPayload {
    let succeeded: HashSet<VendorKind> = fresh.keys().copied().collect();
    let mut merged: ResetPayload = BTreeMap::new();

    for (vendor_str, models) in cached {
        let preserve = match parse_vendor_str(vendor_str) {
            Some(kind) => !succeeded.contains(&kind),
            None => true,
        };
        if preserve {
            merged.insert(vendor_str.clone(), models.clone());
        }
    }
    for (vendor, models) in fresh {
        merged.insert(vendor::vendor_kind_to_str(vendor).to_string(), models);
    }
    merged
}

fn has_reset_coverage_gaps(quotas: &QuotaPayload, resets: &ResetPayload) -> bool {
    quotas.iter().any(|(vendor, models)| {
        let Some(reset_models) = resets.get(vendor) else {
            return true;
        };
        models.keys().any(|name| !reset_models.contains_key(name))
    })
}

/// On ipbr score fetch/parse failure: prefer the previously cached entries
/// when they carry any ipbr-sourced rows, so a transient ipbr outage does
/// not wipe out the last known ranking authority. Fall through to the
/// inventory-only refresh only when no cached ipbr data exists, leaving
/// inventory-visible models present with phase scores `None`. The caller
/// MUST NOT persist the result — letting the next successful refresh write
/// fresh ipbr data without first being suppressed by a cached inv-only
/// snapshot.
fn resolve_score_failure_entries(
    cached: Vec<DashboardEntry>,
    inventory_only: Vec<DashboardEntry>,
) -> Vec<DashboardEntry> {
    if cached
        .iter()
        .any(|entry| entry.score_source == ScoreSource::Ipbr)
    {
        cached
    } else {
        inventory_only
    }
}

fn dashboard_models_to_entries(models: &[dashboard::DashboardModel]) -> Vec<DashboardEntry> {
    models
        .iter()
        .map(|m| DashboardEntry {
            vendor: m.vendor.clone(),
            name: m.name.clone(),
            overall_score: m.overall_score,
            current_score: m.current_score,
            standard_error: m.standard_error,
            axes: m.axes.clone(),
            axis_provenance: m.axis_provenance.clone(),
            ipbr_phase_scores: m.ipbr_phase_scores,
            score_source: m.score_source,
            ipbr_row_matched: m.ipbr_row_matched,
            display_order: m.display_order,
            fallback_from: m.fallback_from.clone(),
        })
        .collect()
}

fn dashboard_warnings_to_quota_errors(warnings: Vec<String>) -> Vec<QuotaError> {
    warnings
        .into_iter()
        .map(|message| QuotaError {
            // Dashboard refresh diagnostics are currently displayed
            // through the shared QuotaError list; Claude is the existing
            // sentinel for dashboard-sourced notices.
            vendor: VendorKind::Claude,
            message: format!("dashboard warning: {message}"),
        })
        .collect()
}

fn parse_vendor_str(s: &str) -> Option<VendorKind> {
    match s {
        "anthropic" | "claude" => Some(VendorKind::Claude),
        "codex" | "openai" => Some(VendorKind::Codex),
        "gemini" | "google" => Some(VendorKind::Gemini),
        "kimi" | "moonshotai" => Some(VendorKind::Kimi),
        _ => None,
    }
}

#[cfg(test)]
mod tests_mod;
