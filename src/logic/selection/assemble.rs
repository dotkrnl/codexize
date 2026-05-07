//! Pure model-universe assembly: given already-fetched dashboard entries,
//! quota maps, and reset maps, produce the canonical `CachedModel` list with
//! Kimi collapse, sibling synthesis, and provenance stamping applied.
//!
//! This module performs NO backend IO. All cache reads/writes, dashboard
//! refresh fetches, and quota refresh fetches live in the data layer
//! (`crate::data::selection_assembly`), which calls into this module after
//! the snapshots have been resolved. The merge / coverage-gap helpers are
//! exposed because that orchestrator needs them between IO calls.
use super::quota;
use super::ranking::stamp_selection_provenance;
use super::types::{CachedModel, QuotaError, ScoreSource, VendorKind};
use super::vendor;
use crate::cache::{DashboardEntry, QuotaPayload, ResetPayload};
use crate::dashboard::{self, DashboardModel};
use std::collections::{BTreeMap, BTreeSet, HashSet};
/// Build the canonical model universe from already-resolved snapshots.
///
/// Pure: callers (the data-layer adapter) are responsible for any cache
/// reads, refresh fetches, and writes that produced these inputs. This
/// function only merges / collapses / ranks them.
pub fn assemble_universe(
    dashboard_entries: Vec<DashboardEntry>,
    quota_payload: QuotaPayload,
    reset_payload: ResetPayload,
    available_vendors: &BTreeSet<VendorKind>,
) -> Vec<CachedModel> {
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
    let mut dashboard_models: Vec<DashboardModel> = dashboard_entries
        .into_iter()
        .filter(|entry| {
            parse_vendor_str(&entry.vendor)
                .is_some_and(|vendor| available_vendors.contains(&vendor))
        })
        .map(|e| DashboardModel {
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
            ipbr_match_key: e.ipbr_match_key,
            route_underlying_vendor: e.route_underlying_vendor,
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
                ipbr_match_key: m.ipbr_match_key,
                route_underlying_vendor: m.route_underlying_vendor,
                quota_percent,
                quota_resets_at,
                display_order: m.display_order,
                fallback_from: m.fallback_from,
            })
        })
        .collect();
    // Collapse all Kimi models into a single "kimi-latest" representative.
    // The canonical pick is the ipbr-matched sibling with the largest sum of
    // present phase scores; ipbr scores are authoritative (unlike cosmetic
    // overall_score / current_score, which selection MUST NOT consult).
    // Display_order and name only break ties — including the no-ipbr case,
    // where every candidate has phase-score sum 0 and falls through to
    // stable inventory order.
    let best_kimi_idx = models
        .iter()
        .enumerate()
        .filter(|(_, m)| m.vendor == VendorKind::Kimi)
        .min_by(|(_, a), (_, b)| {
            let ipbr = |m: &CachedModel| m.score_source == ScoreSource::Ipbr;
            let phase_score_sum = |m: &CachedModel| -> f64 {
                let scores = m.ipbr_phase_scores;
                [scores.idea, scores.planning, scores.build, scores.review]
                    .into_iter()
                    .flatten()
                    .sum()
            };
            // `min_by` picks the smaller key, so reverse the "prefer ipbr"
            // and "prefer higher score" comparisons.
            ipbr(b)
                .cmp(&ipbr(a))
                .then_with(|| {
                    phase_score_sum(b)
                        .partial_cmp(&phase_score_sum(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.display_order.cmp(&b.display_order))
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
    models
}
/// Merge a freshly-fetched quota map (keyed by `VendorKind`) into the cached
/// payload (keyed by vendor string). Successfully-refreshed vendors overwrite
/// cached entries; cached entries for vendors that did not refresh
/// successfully are carried forward (stale-on-error fallback).
pub fn merge_quota_payload(
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
pub fn merge_reset_payload(
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
pub fn has_reset_coverage_gaps(quotas: &QuotaPayload, resets: &ResetPayload) -> bool {
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
pub fn resolve_score_failure_entries(
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
pub fn dashboard_models_to_entries(models: &[DashboardModel]) -> Vec<DashboardEntry> {
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
            ipbr_match_key: m.ipbr_match_key.clone(),
            route_underlying_vendor: m.route_underlying_vendor,
            display_order: m.display_order,
            fallback_from: m.fallback_from.clone(),
        })
        .collect()
}
pub fn dashboard_warnings_to_quota_errors(warnings: Vec<String>) -> Vec<QuotaError> {
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
pub fn parse_vendor_str(s: &str) -> Option<VendorKind> {
    match s {
        "anthropic" | "claude" => Some(VendorKind::Claude),
        "codex" | "openai" => Some(VendorKind::Codex),
        "gemini" | "google" => Some(VendorKind::Gemini),
        "kimi" | "moonshotai" => Some(VendorKind::Kimi),
        "opencode" => Some(VendorKind::Opencode),
        _ => None,
    }
}
#[cfg(test)]
mod tests_mod;
