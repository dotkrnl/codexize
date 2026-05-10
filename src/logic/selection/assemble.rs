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
use super::subscription;
use super::types::{CachedModel, Candidate, CliKind, QuotaError, SubscriptionKind};
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
/// [`baked::merge_with_overrides`] and treats the resulting provider
/// list as the launch inventory. A dashboard/IPBR row is materialized
/// only when at least one resolved provider has an exact
/// `ProviderEntry.model == DashboardEntry.name`; provider `launch_name`
/// is the CLI argument and is not used as a row key.
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

    // Resolve baked + user `[[providers]]` and index by canonical
    // provider model so per-row lookups are linear in the size of the
    // resolved list once. The provider list is the ground truth launch
    // inventory; IPBR supplies scores for matching canonical row names.
    let resolved_providers = baked::merge_with_overrides(providers);
    let providers_by_row = group_providers_by_row(&resolved_providers);
    let ipbr_model_names: BTreeSet<String> = dashboard_entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect();

    let mut rows: BTreeMap<String, CachedModel> = BTreeMap::new();
    let mut consumed_providers: BTreeSet<(SubscriptionKind, String, CliKind, String)> =
        BTreeSet::new();
    for entry in dashboard_entries {
        if !providers_by_row.contains_key(&entry.name) {
            continue;
        }
        let row = rows
            .entry(entry.name.clone())
            .or_insert_with(|| row_from_entry(entry.name.clone(), &entry));
        append_provider_candidates(
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

    let warnings = providers_by_row
        .keys()
        .filter(|model| !ipbr_model_names.contains(*model))
        .map(|model| format!("provider model '{model}' is not present in ipbr"))
        .collect::<Vec<_>>();

    let mut models: Vec<CachedModel> = rows
        .into_values()
        .map(|mut row| {
            refresh_selected_candidate(&mut row);
            row
        })
        .collect();
    models.sort_by(|a, b| {
        a.display_order
            .cmp(&b.display_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    (models, warnings)
}

fn row_from_entry(name: String, entry: &DashboardEntry) -> CachedModel {
    CachedModel {
        subscription: SubscriptionKind::Direct,
        name,
        ipbr_phase_scores: entry.ipbr_phase_scores,
        score_source: entry.score_source,
        candidates: Vec::new(),
        selected_candidate: None,
        quota_percent: None,
        quota_resets_at: None,
        display_order: entry.display_order,
    }
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

/// Append candidates for every resolved provider entry on this canonical
/// model row. The entry's own `subscription` field drives quota lookups.
#[allow(clippy::too_many_arguments)]
fn append_provider_candidates(
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
        row.subscription = candidate.subscription;
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
    let mut free_pool = Vec::new();
    let mut official_pool = Vec::new();
    let mut no_quota_pool = Vec::new();
    let mut non_official_pool = Vec::new();
    for (index, candidate) in candidates.iter().enumerate().filter(|(_, c)| c.enabled) {
        if candidate.free {
            free_pool.push(index);
        } else if candidate.official {
            official_pool.push(index);
        } else if candidate.quota_disabled {
            // `quota_disabled` (force-100%) is the spec's "no-quota"
            // pool — it sits between official-with-good-quota and the
            // non-official pool so the operator can park a self-hosted
            // route here without having to lie about its quota.
            no_quota_pool.push(index);
        } else {
            non_official_pool.push(index);
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
    merged.values.extend(
        cached
            .values
            .iter()
            .filter(|(subscription, _)| should_preserve_cached(subscription, &succeeded))
            .map(|(subscription, models)| (subscription.clone(), models.clone())),
    );
    merged.values.extend(
        fresh
            .into_iter()
            .map(|(kind, models)| (subscription_key(kind), models)),
    );
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
    merged.extend(
        cached
            .iter()
            .filter(|(subscription, _)| should_preserve_cached(subscription, &succeeded))
            .map(|(subscription, models)| (subscription.clone(), models.clone())),
    );
    merged.extend(
        fresh
            .into_iter()
            .map(|(kind, models)| (subscription_key(kind), models)),
    );
    merged
}

fn should_preserve_cached(subscription: &str, succeeded: &HashSet<SubscriptionKind>) -> bool {
    parse_subscription_str(subscription).is_some_and(|kind| !succeeded.contains(&kind))
}

fn subscription_key(kind: SubscriptionKind) -> String {
    subscription::subscription_kind_to_str(kind).to_string()
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
            name: m.name.clone(),
            ipbr_phase_scores: m.ipbr_phase_scores,
            score_source: m.score_source,
            display_order: m.display_order,
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
            subscription: SubscriptionKind::Claude,
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
