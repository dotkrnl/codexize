//! Pure model-universe assembly: given already-fetched dashboard entries,
//! quota maps, and reset maps, produce one model-first row per ipbr canonical
//! name with all known launch candidates attached.
//!
//! This module performs NO backend IO. All cache reads/writes, dashboard
//! refresh fetches, and quota refresh fetches live in the data layer
//! (`crate::data::selection_assembly`), which calls into this module after
//! the snapshots have been resolved. The merge / coverage-gap helpers are
//! exposed because that orchestrator needs them between IO calls.
use super::baked;
use super::ranking::stamp_selection_provenance;
use super::types::{CachedModel, Candidate, CliKind, QuotaError, SubscriptionKind};
use super::vendor;
use crate::cache::{DashboardEntry, QuotaPayload, ResetPayload};
use crate::dashboard::DashboardModel;
use crate::data::config::schema::ProviderEntry;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Per spec §"Selection algorithm" step 2: the official pool only wins
/// outright when its best provider's effective_quota is `>= 21`. Below
/// that floor the official pool is merged with the non-official pool
/// and re-compared. Kept as a named constant so the boundary is grep-
/// able from snapshot diffs.
const OFFICIAL_QUOTA_FLOOR: u8 = 21;

/// Build the canonical model universe from already-resolved snapshots.
///
/// Pure: callers (the data-layer adapter) are responsible for any cache
/// reads, refresh fetches, and writes that produced these inputs. This
/// function only merges, groups, and ranks those snapshots.
///
/// `providers` carries the operator's `[[providers]]` list. Assembly
/// resolves it against the baked defaults via
/// [`baked::merge_with_overrides`] and uses the result to:
///   - override per-tuple flags (`enabled`, `free`, `official`,
///     `quota_disabled`, eligibility, mapping) on each dashboard-row
///     candidate that matches a provider entry by `(cli, launch_name)`;
///   - append user-added candidates whose `model` matches a dashboard
///     row but whose `(cli, launch_name)` differs from the row's
///     natural candidate.
///
/// Returns `(models, warnings)` where `warnings` is currently always
/// empty; the slot is preserved to minimize churn at call sites.
pub fn assemble_universe(
    dashboard_entries: Vec<DashboardEntry>,
    quota_payload: QuotaPayload,
    reset_payload: ResetPayload,
    available_clis: &BTreeSet<CliKind>,
    providers: &[ProviderEntry],
) -> (Vec<CachedModel>, Vec<String>) {
    let failed_subscriptions = quota_payload.failed_subscriptions.clone();
    // Keep all parseable subscriptions in the parsed quota/reset maps;
    // selection's per-candidate filter (`available_clis`) handles
    // availability now, so we don't drop subscription rows here.
    let parsed_quotas: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>> = quota_payload
        .values
        .into_iter()
        .filter_map(|(subscription_name, models)| {
            parse_subscription_str(&subscription_name).map(|subscription| (subscription, models))
        })
        .collect();
    let parsed_resets: BTreeMap<
        SubscriptionKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    > = reset_payload
        .into_iter()
        .filter_map(|(subscription_name, models)| {
            parse_subscription_str(&subscription_name).map(|subscription| (subscription, models))
        })
        .collect();

    // Resolve baked + user `[[providers]]` and index by the dashboard's
    // (subscription, model) key so per-row lookups are linear in the
    // size of the resolved list once. `provider_index_for` returns
    // entries in baked-then-additions order, preserving display_order
    // intent for additions that are appended below.
    let resolved_providers = baked::merge_with_overrides(providers);
    let providers_by_row = group_providers_by_row(&resolved_providers);

    let mut rows: BTreeMap<String, CachedModel> = BTreeMap::new();
    let mut consumed_providers: BTreeSet<(SubscriptionKind, String, CliKind, String)> =
        BTreeSet::new();
    for entry in dashboard_entries {
        let Some(row_name) = row_name_for_entry(&entry) else {
            continue;
        };
        let dashboard_subscription = subscription_for_dashboard(&entry);
        let candidate = dashboard_subscription.and_then(|subscription| {
            build_candidate(
                subscription,
                &entry,
                entry.display_order,
                &parsed_quotas,
                &parsed_resets,
                &failed_subscriptions,
                available_clis,
                &providers_by_row,
            )
        });
        let row = rows
            .entry(row_name.clone())
            .or_insert_with(|| row_from_entry(row_name.clone(), &entry));
        if let Some(candidate) = candidate {
            // With `subscription_for_dashboard` keyed off the baked
            // table, multiple dashboard rows for the same model can
            // resolve to the same subscription (e.g. an opencode
            // dashboard row whose model is baked-owned by Claude).
            // Dedupe on the candidate identity so the row collects one
            // candidate per `(subscription, cli, launch_name)`.
            let key = (
                candidate.subscription,
                entry.name.clone(),
                candidate.cli,
                candidate.launch_name.clone(),
            );
            if consumed_providers.insert(key) {
                row.candidates.push(candidate);
            }
        }
        // Append user-added providers keyed by this row's model whose
        // `(cli, launch_name)` differs from the natural dashboard
        // candidate. These never reach `build_candidate` — they
        // originate from `[[providers]]`, not from a dashboard entry —
        // so we materialise them here off the resolved list.
        append_provider_additions(
            row,
            &entry.name,
            &providers_by_row,
            &parsed_quotas,
            &parsed_resets,
            &failed_subscriptions,
            available_clis,
            &mut consumed_providers,
        );
    }

    let free_model_warnings: Vec<String> = Vec::new();

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
        subscription_for_dashboard(entry).map(|_| entry.name.clone())
    }
}

fn row_from_entry(name: String, entry: &DashboardEntry) -> CachedModel {
    CachedModel {
        vendor: SubscriptionKind::Direct,
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

/// Resolve the subscription that "owns" a dashboard row. The baked table
/// is consulted first so curated overrides (e.g. ipbr-aliased Kimi names
/// that the dashboard surfaces under `moonshotai`) honour our hand-picked
/// primary subscription. When the row is not in the baked table we fall
/// back to parsing the dashboard's own vendor string.
pub fn subscription_for_dashboard(entry: &DashboardEntry) -> Option<SubscriptionKind> {
    if let Some(sub) = baked::primary_subscription_for_model(&entry.name) {
        return Some(sub);
    }
    parse_subscription_str(&entry.vendor)
}

/// Index of resolved providers (baked ⊕ user) keyed by `model`. Each
/// key maps to the providers that belong on that row, in the order
/// produced by [`baked::merge_with_overrides`] (baked first, additions
/// last). Identity is `(cli, launch_name)`; subscription is a property
/// of the entry, not the row key.
type ProvidersByRow<'a> = BTreeMap<String, Vec<&'a ProviderEntry>>;

fn group_providers_by_row(providers: &[ProviderEntry]) -> ProvidersByRow<'_> {
    let mut by_row: ProvidersByRow<'_> = BTreeMap::new();
    for entry in providers {
        by_row.entry(entry.model.clone()).or_default().push(entry);
    }
    by_row
}

#[allow(clippy::too_many_arguments)]
fn build_candidate(
    subscription: SubscriptionKind,
    dashboard_entry: &DashboardEntry,
    display_order: usize,
    parsed_quotas: &BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
    parsed_resets: &BTreeMap<
        SubscriptionKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    >,
    failed_subscriptions: &BTreeSet<SubscriptionKind>,
    available_clis: &BTreeSet<CliKind>,
    providers_by_row: &ProvidersByRow<'_>,
) -> Option<Candidate> {
    let cli = match subscription {
        SubscriptionKind::OpencodeGo => CliKind::Opencode,
        SubscriptionKind::Direct => return None,
        direct => direct.direct_cli()?,
    };
    if !available_clis.contains(&cli) {
        return None;
    }
    let dashboard_name = dashboard_entry.name.as_str();
    let launch_name = dashboard_name;
    // Strict baked-only path: a dashboard row produces no candidate
    // unless the resolved (baked ⊕ user) provider list has an entry
    // matching `(cli, launch_name)` keyed by this row's model. Without a
    // hit we drop the candidate — there is no synthesized fallback.
    let resolved = providers_by_row
        .get(&dashboard_name.to_string())
        .and_then(|entries| {
            entries
                .iter()
                .copied()
                .find(|entry| entry.cli == cli && entry.launch_name == launch_name)
        })
        .cloned()?;
    Some(make_candidate(
        subscription,
        cli,
        launch_name,
        display_order,
        &resolved,
        parsed_quotas,
        parsed_resets,
        failed_subscriptions,
    ))
}

/// Materialise a `Candidate` with quota/reset lookups applied. The
/// per-tuple flags and effort mapping come from `props` (a resolved
/// `ProviderEntry`); the caller is responsible for picking the right
/// entry. Quota and reset lookups consult `props.quota_lookup_key` first
/// (so e.g. Kimi/OpencodeGo entries can point at their shared-pool
/// sentinel) and fall back to the launch name itself.
#[allow(clippy::too_many_arguments)]
fn make_candidate(
    subscription: SubscriptionKind,
    cli: CliKind,
    launch_name: &str,
    display_order: usize,
    props: &ProviderEntry,
    parsed_quotas: &BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
    parsed_resets: &BTreeMap<
        SubscriptionKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    >,
    failed_subscriptions: &BTreeSet<SubscriptionKind>,
) -> Candidate {
    let lookup_key = props.quota_lookup_key.as_deref().unwrap_or(launch_name);
    let quota_percent = parsed_quotas
        .get(&subscription)
        .and_then(|by_model| by_model.get(lookup_key))
        .copied()
        .flatten();
    let quota_resets_at = parsed_resets
        .get(&subscription)
        .and_then(|by_model| by_model.get(lookup_key))
        .copied()
        .flatten();
    let quota_failed = failed_subscriptions.contains(&subscription);
    Candidate {
        subscription,
        cli,
        launch_name: launch_name.to_string(),
        quota_percent,
        quota_resets_at,
        display_order,
        enabled: props.enabled,
        free: props.free,
        official: props.official,
        quota_disabled: props.quota_disabled,
        cheap_eligible: props.cheap_eligible,
        tough_eligible: props.tough_eligible,
        effort_eligible: props.effort_eligible,
        effort_mapping: props.effort_mapping.clone(),
        quota_failed,
    }
}

/// Append candidates for any provider entries on this row whose
/// `(cli, launch_name)` doesn't match the row's natural dashboard
/// candidate. The entry's own `subscription` field drives quota lookups.
#[allow(clippy::too_many_arguments)]
fn append_provider_additions(
    row: &mut CachedModel,
    dashboard_model: &str,
    providers_by_row: &ProvidersByRow<'_>,
    parsed_quotas: &BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
    parsed_resets: &BTreeMap<
        SubscriptionKind,
        BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>,
    >,
    failed_subscriptions: &BTreeSet<SubscriptionKind>,
    available_clis: &BTreeSet<CliKind>,
    consumed: &mut BTreeSet<(SubscriptionKind, String, CliKind, String)>,
) {
    let Some(entries) = providers_by_row.get(dashboard_model) else {
        return;
    };
    for entry in entries {
        let candidate_subscription = entry.subscription;
        let key = (
            candidate_subscription,
            dashboard_model.to_string(),
            entry.cli,
            entry.launch_name.clone(),
        );
        if !consumed.insert(key) {
            continue;
        }
        if !available_clis.contains(&entry.cli) {
            continue;
        }
        row.candidates.push(make_candidate(
            candidate_subscription,
            entry.cli,
            &entry.launch_name,
            entry.display_order as usize,
            entry,
            parsed_quotas,
            parsed_resets,
            failed_subscriptions,
        ));
    }
}

fn refresh_selected_candidate(row: &mut CachedModel) {
    row.selected_candidate = select_candidate_index(&row.candidates);
    let selected = row.selected_candidate().cloned();
    if let Some(candidate) = selected {
        row.vendor = candidate.subscription;
        // The row-level quota tracks the *selected* provider's effective
        // quota (per-spec: free/quota_disabled = 100, fetch failure = 50,
        // unknown = None). Downstream consumers (UI, candidate-pool
        // weighting in `ranking.rs`) read `row.quota_percent` — keeping
        // this aligned with the selected candidate keeps them consistent
        // with the provider the run actually launches.
        row.quota_percent = candidate.effective_quota();
        row.quota_resets_at = candidate.quota_resets_at;
    }
}

/// Step 2 of the spec's two-step selection: with the row already chosen
/// (step 1 happens at the model-pool level via dashboard score in
/// `super::selection`), pick the best provider inside the row using the
/// Priority ladder `free > official(>=21) > no-quota > non-official`.
pub fn select_candidate_index(candidates: &[Candidate]) -> Option<usize> {
    let enabled: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| candidate.enabled.then_some(index))
        .collect();
    if enabled.is_empty() {
        return None;
    }

    let mut free_pool = Vec::new();
    let mut official_pool = Vec::new();
    let mut no_quota_pool = Vec::new();
    let mut non_official_pool = Vec::new();
    for index in &enabled {
        let candidate = &candidates[*index];
        if candidate.free {
            free_pool.push(*index);
        } else if candidate.official {
            official_pool.push(*index);
        } else if candidate.quota_disabled {
            // `quota_disabled` (force-100%) is the spec's "no-quota"
            // pool — it sits between official-with-good-quota and the
            // non-official pool so the operator can park a self-hosted
            // route here without having to lie about its quota.
            no_quota_pool.push(*index);
        } else {
            non_official_pool.push(*index);
        }
    }

    if !free_pool.is_empty() {
        return min_by_compare(&free_pool, candidates);
    }
    if !official_pool.is_empty() {
        let best_official = min_by_compare(&official_pool, candidates).expect("non-empty pool");
        let best_quota = candidates[best_official].effective_quota().unwrap_or(0);
        if best_quota >= OFFICIAL_QUOTA_FLOOR {
            return Some(best_official);
        }
    }
    if !no_quota_pool.is_empty() {
        return min_by_compare(&no_quota_pool, candidates);
    }
    let mut merged = official_pool;
    merged.extend(non_official_pool.iter().copied());
    min_by_compare(&merged, candidates)
}

fn min_by_compare(indices: &[usize], candidates: &[Candidate]) -> Option<usize> {
    indices
        .iter()
        .copied()
        .min_by(|a, b| compare_provider(&candidates[*a], &candidates[*b]))
}

fn compare_provider(a: &Candidate, b: &Candidate) -> Ordering {
    compare_quota_for_candidate(a.effective_quota(), b.effective_quota())
        .then_with(|| a.display_order.cmp(&b.display_order))
        .then_with(|| a.launch_name.cmp(&b.launch_name))
}

fn compare_quota_for_candidate(a: Option<u8>, b: Option<u8>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Merge a freshly-fetched quota map (keyed by `SubscriptionKind`) into the cached
/// payload (keyed by subscription string). Successfully-refreshed subscriptions overwrite
/// cached entries; cached entries for subscriptions that did not refresh
/// successfully are carried forward (stale-on-error fallback).
///
/// `failed` is the set of subscriptions whose fresh fetch errored in this
/// refresh round. They are recorded in `merged.failed_subscriptions` so
/// the selection layer can apply the spec's 50% capacity assumption.
/// Subscriptions that were refreshed successfully clear any prior failure
/// marker; subscriptions not touched in this round preserve theirs.
pub fn merge_quota_payload(
    cached: &QuotaPayload,
    fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
    failed: &BTreeSet<SubscriptionKind>,
) -> QuotaPayload {
    let succeeded: HashSet<SubscriptionKind> = fresh.keys().copied().collect();
    let mut merged = QuotaPayload::default();
    for (subscription_str, models) in &cached.values {
        // Drop cache entries whose subscription string no longer parses
        // (e.g. legacy "free" rows from a previous schema). Tracked
        // subscriptions either get refreshed-in below or carry forward
        // their cached value.
        let preserve = match parse_subscription_str(subscription_str) {
            Some(kind) => !succeeded.contains(&kind),
            None => false,
        };
        if preserve {
            merged
                .values
                .insert(subscription_str.clone(), models.clone());
        }
    }
    for (subscription, models) in fresh {
        merged.values.insert(
            vendor::subscription_kind_to_str(subscription).to_string(),
            models,
        );
    }
    // Re-derive `failed_subscriptions` for this round: drop markers
    // belonging to subscriptions that just refreshed cleanly, keep
    // markers for subscriptions we did not touch, and add fresh markers
    // for subscriptions that errored.
    let mut failed_set: BTreeSet<SubscriptionKind> = cached
        .failed_subscriptions
        .iter()
        .copied()
        .filter(|kind| !succeeded.contains(kind))
        .collect();
    for kind in failed {
        failed_set.insert(*kind);
    }
    merged.failed_subscriptions = failed_set;
    merged
}
pub fn merge_reset_payload(
    cached: &ResetPayload,
    fresh: BTreeMap<SubscriptionKind, BTreeMap<String, Option<chrono::DateTime<chrono::Utc>>>>,
) -> ResetPayload {
    let succeeded: HashSet<SubscriptionKind> = fresh.keys().copied().collect();
    let mut merged: ResetPayload = BTreeMap::new();
    for (subscription_str, models) in cached {
        // Mirror `merge_quota_payload`: unparseable subscription keys are
        // stale and dropped on the next refresh.
        let preserve = match parse_subscription_str(subscription_str) {
            Some(kind) => !succeeded.contains(&kind),
            None => false,
        };
        if preserve {
            merged.insert(subscription_str.clone(), models.clone());
        }
    }
    for (subscription, models) in fresh {
        merged.insert(
            vendor::subscription_kind_to_str(subscription).to_string(),
            models,
        );
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
        "kimi" | "moonshot" | "moonshotai" => Some(SubscriptionKind::Kimi),
        "opencode" | "opencode-go" => Some(SubscriptionKind::OpencodeGo),
        "direct" => Some(SubscriptionKind::Direct),
        _ => None,
    }
}
#[cfg(test)]
mod tests_mod;
