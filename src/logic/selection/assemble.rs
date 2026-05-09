//! Pure model-universe assembly: given already-fetched dashboard entries,
//! quota maps, and reset maps, produce one model-first row per ipbr canonical
//! name with all known launch candidates attached.
//!
//! This module performs NO backend IO. All cache reads/writes, dashboard
//! refresh fetches, and quota refresh fetches live in the data layer
//! (`crate::data::selection_assembly`), which calls into this module after
//! the snapshots have been resolved. The merge / coverage-gap helpers are
//! exposed because that orchestrator needs them between IO calls.
use super::quota;
use super::ranking::stamp_selection_provenance;
use super::types::{CachedModel, Candidate, CliKind, FreeModelEntry, QuotaError, SubscriptionKind};
use super::vendor;
use crate::cache::{DashboardEntry, QuotaPayload, ResetPayload};
use crate::dashboard::DashboardModel;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};

const DIRECT_QUOTA_FLOOR: u8 = 20;

/// Build the canonical model universe from already-resolved snapshots.
///
/// Pure: callers (the data-layer adapter) are responsible for any cache
/// reads, refresh fetches, and writes that produced these inputs. This
/// function only merges, groups, and ranks those snapshots.
///
/// Returns `(models, free_model_warnings)` where `free_model_warnings`
/// lists `mapped_into` values from `free_models` entries that did not
/// match any ipbr canonical row name.
pub fn assemble_universe(
    dashboard_entries: Vec<DashboardEntry>,
    quota_payload: QuotaPayload,
    reset_payload: ResetPayload,
    available_subscriptions: &BTreeSet<SubscriptionKind>,
    free_models: &[FreeModelEntry],
) -> (Vec<CachedModel>, Vec<String>) {
    let parsed_quotas: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>> = quota_payload
        .values
        .into_iter()
        .filter_map(|(subscription_name, models)| {
            parse_subscription_str(&subscription_name).map(|subscription| (subscription, models))
        })
        .filter(|(subscription, _)| available_subscriptions.contains(subscription))
        .collect();
    let parsed_resets: BTreeMap<
        SubscriptionKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    > = reset_payload
        .into_iter()
        .filter_map(|(subscription_name, models)| {
            parse_subscription_str(&subscription_name).map(|subscription| (subscription, models))
        })
        .filter(|(subscription, _)| available_subscriptions.contains(subscription))
        .collect();

    let mut rows: BTreeMap<String, CachedModel> = BTreeMap::new();
    for entry in dashboard_entries {
        let Some(row_name) = row_name_for_entry(&entry) else {
            continue;
        };
        let subscription = parse_dashboard_subscription(&entry);
        let candidate = subscription.and_then(|subscription| {
            build_candidate(
                subscription,
                &entry.name,
                entry.display_order,
                &parsed_quotas,
                &parsed_resets,
                available_subscriptions,
            )
        });
        let row = rows
            .entry(row_name.clone())
            .or_insert_with(|| row_from_entry(row_name.clone(), &entry));
        if let Some(candidate) = candidate {
            row.candidates.push(candidate);
        }
    }

    for free in free_models {
        if let Some(row) = rows.get_mut(&free.mapped_into) {
            row.candidates.push(Candidate {
                subscription: SubscriptionKind::Free,
                cli: free.cli,
                launch_name: free.model_name.clone(),
                quota_percent: Some(100),
                quota_resets_at: None,
                display_order: row.display_order,
            });
        }
        // Unmatched mapped_into entries are excluded from candidates
        // and surfaced as soft warnings below.
    }

    let free_model_warnings: Vec<String> = free_models
        .iter()
        .filter(|free| !rows.contains_key(&free.mapped_into))
        .map(|free| {
            format!(
                "free_models entry mapped_into {:?} does not match any ipbr row",
                free.mapped_into
            )
        })
        .collect();

    let mut models: Vec<CachedModel> = rows
        .into_values()
        .map(|mut row| {
            refresh_selected_candidate(&mut row);
            stamp_selection_provenance(&mut row);
            row
        })
        .collect();
    models.sort_by(|a, b| {
        a.display_order
            .cmp(&b.display_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    (models, free_model_warnings)
}

fn row_name_for_entry(entry: &DashboardEntry) -> Option<String> {
    if entry.ipbr_row_matched {
        Some(
            entry
                .ipbr_match_key
                .clone()
                .unwrap_or_else(|| entry.name.clone()),
        )
    } else {
        parse_dashboard_subscription(entry).map(|_| entry.name.clone())
    }
}

fn row_from_entry(name: String, entry: &DashboardEntry) -> CachedModel {
    CachedModel {
        vendor: SubscriptionKind::Free,
        name,
        overall_score: entry.overall_score,
        current_score: entry.current_score,
        standard_error: entry.standard_error,
        axes: entry.axes.clone(),
        axis_provenance: entry.axis_provenance.clone(),
        ipbr_phase_scores: entry.ipbr_phase_scores,
        score_source: entry.score_source,
        ipbr_row_matched: entry.ipbr_row_matched,
        ipbr_match_key: entry.ipbr_match_key.clone(),
        candidates: Vec::new(),
        selected_candidate: None,
        quota_percent: None,
        quota_resets_at: None,
        display_order: entry.display_order,
        fallback_from: entry.fallback_from.clone(),
    }
}

fn parse_dashboard_subscription(entry: &DashboardEntry) -> Option<SubscriptionKind> {
    if entry.vendor == "opencode" {
        Some(SubscriptionKind::OpencodeGo)
    } else {
        parse_subscription_str(&entry.vendor)
    }
}

fn build_candidate(
    subscription: SubscriptionKind,
    dashboard_name: &str,
    display_order: usize,
    parsed_quotas: &BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
    parsed_resets: &BTreeMap<
        SubscriptionKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    >,
    available_subscriptions: &BTreeSet<SubscriptionKind>,
) -> Option<Candidate> {
    if !available_subscriptions.contains(&subscription) {
        return None;
    }
    let cli = match subscription {
        SubscriptionKind::OpencodeGo => CliKind::Opencode,
        SubscriptionKind::Free => return None,
        direct => direct.direct_cli()?,
    };
    let launch_name = if subscription == SubscriptionKind::Kimi
        && parsed_quotas
            .get(&SubscriptionKind::Kimi)
            .is_some_and(|models| models.contains_key("kimi-latest"))
    {
        "kimi-latest"
    } else {
        dashboard_name
    };
    let quota_percent = parsed_quotas
        .get(&subscription)
        .and_then(|by_model| by_model.get(launch_name))
        .copied()
        .flatten()
        .or_else(|| quota::find_quota_by_heuristic(launch_name, subscription, parsed_quotas))
        .or_else(|| {
            (launch_name != dashboard_name)
                .then(|| {
                    quota::find_quota_by_heuristic(dashboard_name, subscription, parsed_quotas)
                })
                .flatten()
        });
    let quota_resets_at = parsed_resets
        .get(&subscription)
        .and_then(|by_model| by_model.get(launch_name))
        .copied()
        .flatten()
        .or_else(|| quota::find_reset_by_heuristic(launch_name, subscription, parsed_resets));
    Some(Candidate {
        subscription,
        cli,
        launch_name: launch_name.to_string(),
        quota_percent,
        quota_resets_at,
        display_order,
    })
}

fn refresh_selected_candidate(row: &mut CachedModel) {
    row.selected_candidate = select_candidate_index(&row.candidates);
    let selected = row.selected_candidate().cloned();
    if let Some(candidate) = selected {
        row.vendor = candidate.subscription;
        row.quota_percent = candidate.quota_percent;
        row.quota_resets_at = candidate.quota_resets_at;
    }
}

pub(crate) fn select_candidate_index(candidates: &[Candidate]) -> Option<usize> {
    let eligible: Vec<usize> = if candidates.iter().any(|candidate| {
        candidate.subscription != SubscriptionKind::OpencodeGo
            && matches!(candidate.quota_percent, Some(quota) if quota >= DIRECT_QUOTA_FLOOR)
    }) {
        candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                (candidate.subscription != SubscriptionKind::OpencodeGo).then_some(index)
            })
            .collect()
    } else {
        (0..candidates.len()).collect()
    };

    eligible
        .into_iter()
        .min_by(|a, b| compare_candidate(&candidates[*a], &candidates[*b]))
}

fn compare_candidate(a: &Candidate, b: &Candidate) -> Ordering {
    compare_quota_for_candidate(a.quota_percent, b.quota_percent)
        .then_with(|| {
            subscription_tiebreaker(a.subscription).cmp(&subscription_tiebreaker(b.subscription))
        })
        .then_with(|| a.display_order.cmp(&b.display_order))
        .then_with(|| a.launch_name.cmp(&b.launch_name))
}

fn subscription_tiebreaker(subscription: SubscriptionKind) -> u8 {
    match subscription {
        SubscriptionKind::Free => 0,
        SubscriptionKind::OpencodeGo => 2,
        SubscriptionKind::Claude
        | SubscriptionKind::Codex
        | SubscriptionKind::Gemini
        | SubscriptionKind::Kimi => 1,
    }
}

fn compare_quota_for_candidate(a: Option<u8>, b: Option<u8>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
fn parse_kimi_semver(name: &str) -> Option<(u64, u64)> {
    let lower = name.to_ascii_lowercase();
    let after_prefix = lower.strip_prefix("kimi-k")?;
    let (major_str, minor_str) = after_prefix.split_once('.').unwrap_or((after_prefix, ""));
    if major_str.is_empty() || !major_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !minor_str.is_empty() && !minor_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((
        major_str.parse().ok()?,
        if minor_str.is_empty() {
            0
        } else {
            minor_str.parse().ok()?
        },
    ))
}

/// Merge a freshly-fetched quota map (keyed by `SubscriptionKind`) into the cached
/// payload (keyed by subscription string). Successfully-refreshed subscriptions overwrite
/// cached entries; cached entries for subscriptions that did not refresh
/// successfully are carried forward (stale-on-error fallback).
pub fn merge_quota_payload(
    cached: &QuotaPayload,
    fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
) -> QuotaPayload {
    let succeeded: HashSet<SubscriptionKind> = fresh.keys().copied().collect();
    let mut merged = QuotaPayload::default();
    for (subscription_str, models) in &cached.values {
        let preserve = match parse_subscription_str(subscription_str) {
            Some(kind) => !succeeded.contains(&kind),
            None => true,
        };
        if preserve {
            merged
                .values
                .insert(subscription_str.clone(), models.clone());
        }
    }
    for (subscription, models) in fresh {
        merged
            .values
            .insert(vendor::vendor_kind_to_str(subscription).to_string(), models);
    }
    // Subscriptions whose fresh fetch did not return a map are
    // considered failed for this round; preserve any prior failure
    // markers for subscriptions we did not refresh in this call.
    merged.failed_subscriptions = cached
        .failed_subscriptions
        .iter()
        .copied()
        .filter(|kind| !succeeded.contains(kind))
        .collect();
    merged
}
pub fn merge_reset_payload(
    cached: &ResetPayload,
    fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>>,
) -> ResetPayload {
    let succeeded: HashSet<SubscriptionKind> = fresh.keys().copied().collect();
    let mut merged: ResetPayload = BTreeMap::new();
    for (subscription_str, models) in cached {
        let preserve = match parse_subscription_str(subscription_str) {
            Some(kind) => !succeeded.contains(&kind),
            None => true,
        };
        if preserve {
            merged.insert(subscription_str.clone(), models.clone());
        }
    }
    for (subscription, models) in fresh {
        merged.insert(vendor::vendor_kind_to_str(subscription).to_string(), models);
    }
    merged
}
pub fn has_reset_coverage_gaps(quotas: &QuotaPayload, resets: &ResetPayload) -> bool {
    quotas.values.iter().any(|(subscription, models)| {
        let Some(reset_models) = resets.get(subscription) else {
            return true;
        };
        models.keys().any(|name| !reset_models.contains_key(name))
    })
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
            vendor: SubscriptionKind::Claude,
            message: format!("dashboard warning: {message}"),
        })
        .collect()
}
pub fn parse_subscription_str(s: &str) -> Option<SubscriptionKind> {
    match s {
        "anthropic" | "claude" => Some(SubscriptionKind::Claude),
        "codex" | "openai" => Some(SubscriptionKind::Codex),
        "gemini" | "google" => Some(SubscriptionKind::Gemini),
        "kimi" | "moonshotai" => Some(SubscriptionKind::Kimi),
        "opencode" | "opencode-go" => Some(SubscriptionKind::OpencodeGo),
        "free" => Some(SubscriptionKind::Free),
        _ => None,
    }
}
#[cfg(test)]
mod tests_mod;
