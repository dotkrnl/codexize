use crate::dashboard::DashboardModel;
use crate::selection::{IpbrPhaseScores, ScoreSource};
use std::collections::{BTreeMap, BTreeSet, HashMap};
#[derive(Debug, Clone)]
pub(crate) struct InventoryEntry {
    pub(crate) name: String,
    pub(crate) vendor: String,
}
/// Canonicalized score record produced by ipbr ingestion. The `name`
/// field uses inventory-compatible `trim().to_ascii_lowercase()` shape so
/// the opencode-inventory merge keys join cleanly; richer normalization is
/// exposed via `canonical_id` / `aliases` for the alias matcher.
/// All production rows are `score_source = Ipbr` and `ipbr_row_matched = true`.
#[derive(Debug, Clone)]
pub(crate) struct ScoreEntry {
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) display_order: usize,
    /// Normalized canonical id from the ipbr feed, when present. Carried
    /// through ingestion for the upcoming normalized-exact matching
    /// layer; the current merge still keys on `name` only, so production
    /// callers do not yet read it. Verified by `dashboard::tests`.
    #[allow(dead_code)]
    pub(crate) canonical_id: Option<String>,
    /// Normalized alias keys from the ipbr feed. Same usage notes as
    /// `canonical_id`.
    #[allow(dead_code)]
    pub(crate) aliases: Vec<String>,
    pub(crate) ipbr_phase_scores: IpbrPhaseScores,
    pub(crate) score_source: ScoreSource,
    pub(crate) ipbr_row_matched: bool,
}
#[derive(Debug, Clone)]
pub(crate) struct MergeResult {
    pub(crate) models: Vec<DashboardModel>,
    pub(crate) warnings: Vec<String>,
}
/// Normalize an ipbr lookup key per spec §"Model Matching":
/// lowercase, replace runs of `.`, `_`, `/`, and ASCII whitespace with
/// `-`, collapse repeated `-`, then trim leading/trailing `-`.
pub(crate) fn normalize_ipbr_key(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_dash = true; // suppresses any leading `-`
    for ch in input.chars() {
        let mapped = if ch.is_ascii_uppercase() {
            ch.to_ascii_lowercase()
        } else {
            ch
        };
        let is_separator = matches!(mapped, '.' | '_' | '/' | '-')
            || (mapped.is_ascii() && mapped.is_ascii_whitespace());
        if is_separator {
            if !last_was_dash {
                out.push('-');
                last_was_dash = true;
            }
        } else {
            out.push(mapped);
            last_was_dash = false;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}
#[cfg(test)]
pub(crate) fn merge(
    inventory: Vec<InventoryEntry>,
    scores: Vec<ScoreEntry>,
) -> Vec<DashboardModel> {
    merge_with_warnings(inventory, scores).models
}
pub(crate) fn merge_with_warnings(
    inventory: Vec<InventoryEntry>,
    scores: Vec<ScoreEntry>,
) -> MergeResult {
    let ipbr_lookup = build_ipbr_lookup(&scores);
    let mut consumed_ipbr_scores = BTreeSet::new();
    let mut models: Vec<DashboardModel> = Vec::with_capacity(inventory.len());
    // Inventory rows (opencode) only survive when they match an ipbr score.
    // Anything without a match is outside the supported universe and is
    // dropped — there is no longer a non-ipbr inventory source.
    //
    // Two-pass match: a strong key (display_name / canonical_id) claims the
    // ipbr row exclusively before any alias resolves. This stops a "less
    // specific" alias on row X from giving X's scores to a separate
    // inventory id whose canonical row is something else — e.g. `glm-5` is
    // listed as an alias of `glm-5.1` upstream, but opencode advertises
    // both ids as distinct routes, so `glm-5` must drop rather than borrow
    // `glm-5.1`'s authority once `glm-5.1` itself has been merged.
    let mut alias_pending: Vec<InventoryEntry> = Vec::new();
    for inv in inventory {
        let key = normalize_ipbr_key(&inv.name);
        match ipbr_lookup.strong.get(&key) {
            Some(&score_index) => {
                if consumed_ipbr_scores.insert(score_index) {
                    push_merged_inventory(&mut models, inv, &scores, score_index);
                }
            }
            None => alias_pending.push(inv),
        }
    }
    for inv in alias_pending {
        let key = normalize_ipbr_key(&inv.name);
        let Some(&score_index) = ipbr_lookup.weak.get(&key) else {
            continue;
        };
        if !consumed_ipbr_scores.insert(score_index) {
            continue;
        }
        push_merged_inventory(&mut models, inv, &scores, score_index);
    }
    let inv_names: std::collections::HashSet<String> =
        models.iter().map(|m| m.name.clone()).collect();
    for (score_index, sc) in scores.iter().enumerate() {
        if !consumed_ipbr_scores.contains(&score_index) && !inv_names.contains(&sc.name) {
            models.push(dashboard_model_from_score(sc.name.clone(), &sc.vendor, sc));
        }
    }
    models.sort_by_key(|m| m.display_order);
    MergeResult {
        models,
        warnings: ipbr_lookup.warnings,
    }
}
fn dashboard_model_from_score(
    name: String,
    inventory_vendor: &str,
    sc: &ScoreEntry,
) -> DashboardModel {
    DashboardModel {
        name,
        dashboard_vendor: if !inventory_vendor.is_empty() {
            inventory_vendor.to_string()
        } else {
            sc.vendor.clone()
        },
        ipbr_phase_scores: sc.ipbr_phase_scores,
        score_source: sc.score_source,
        ipbr_row_matched: sc.ipbr_row_matched,
        ipbr_match_key: if sc.ipbr_row_matched {
            Some(normalize_ipbr_key(&sc.name))
        } else {
            None
        },
        display_order: sc.display_order,
    }
}
struct IpbrLookup {
    /// Lookup by canonical identifiers — display_name and canonical_id.
    /// Inventory rows resolved via this map are treated as the authoritative
    /// owner of the ipbr row, blocking any later alias-based duplicate.
    strong: HashMap<String, usize>,
    /// Lookup by aliases only. Used as a fallback when no strong key matched
    /// the inventory name AND the score row hasn't already been claimed.
    weak: HashMap<String, usize>,
    warnings: Vec<String>,
}
fn build_ipbr_lookup(scores: &[ScoreEntry]) -> IpbrLookup {
    let strong = build_key_map(scores, strong_keys_for_score);
    let weak = build_key_map(scores, alias_keys_for_score);
    // Cross-source collision: a key that resolves to one row via strong keys
    // and a different row via aliases is upstream noise. Drop it from both
    // maps and warn so operators can see the feed problem, mirroring the
    // single-source collision behavior.
    let mut cross_collisions: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (key, &strong_index) in &strong.matches {
        if let Some(&weak_index) = weak.matches.get(key)
            && weak_index != strong_index
        {
            cross_collisions
                .entry(key.clone())
                .or_default()
                .extend([strong_index, weak_index]);
        }
    }
    let mut strong_map = strong.matches;
    let mut weak_map = weak.matches;
    for key in cross_collisions.keys() {
        strong_map.remove(key);
        weak_map.remove(key);
    }
    let mut warnings = strong.warnings;
    warnings.extend(weak.warnings);
    warnings.extend(cross_collisions.into_iter().map(|(key, row_indexes)| {
        let rows = row_indexes
            .into_iter()
            .map(|index| scores[index].name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!("ipbr normalized key '{key}' collided across rows: {rows}; key ignored")
    }));
    IpbrLookup {
        strong: strong_map,
        weak: weak_map,
        warnings,
    }
}
struct KeyMap {
    matches: HashMap<String, usize>,
    warnings: Vec<String>,
}
fn build_key_map<F>(scores: &[ScoreEntry], keys_fn: F) -> KeyMap
where
    F: Fn(&ScoreEntry) -> BTreeSet<String>,
{
    let mut owners: HashMap<String, usize> = HashMap::new();
    let mut collisions: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (index, score) in scores.iter().enumerate() {
        if score.score_source != ScoreSource::Ipbr {
            continue;
        }
        for key in keys_fn(score) {
            match owners.get(&key).copied() {
                Some(owner) if owner != index => {
                    collisions.entry(key).or_default().extend([owner, index]);
                }
                Some(_) => {}
                None => {
                    owners.insert(key, index);
                }
            }
        }
    }
    for key in collisions.keys() {
        owners.remove(key);
    }
    let warnings = collisions
        .into_iter()
        .map(|(key, row_indexes)| {
            let rows = row_indexes
                .into_iter()
                .map(|index| scores[index].name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("ipbr normalized key '{key}' collided across rows: {rows}; key ignored")
        })
        .collect();
    KeyMap {
        matches: owners,
        warnings,
    }
}
fn strong_keys_for_score(score: &ScoreEntry) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let display_key = normalize_ipbr_key(&score.name);
    if !display_key.is_empty() {
        keys.insert(display_key);
    }
    if let Some(canonical_id) = &score.canonical_id {
        let key = normalize_ipbr_key(canonical_id);
        if !key.is_empty() {
            keys.insert(key);
        }
    }
    keys
}
fn alias_keys_for_score(score: &ScoreEntry) -> BTreeSet<String> {
    score
        .aliases
        .iter()
        .map(|alias| normalize_ipbr_key(alias))
        .filter(|key| !key.is_empty())
        .collect()
}
fn push_merged_inventory(
    models: &mut Vec<DashboardModel>,
    inv: InventoryEntry,
    scores: &[ScoreEntry],
    score_index: usize,
) {
    models.push(dashboard_model_from_score(
        inv.name,
        &inv.vendor,
        &scores[score_index],
    ));
}
#[cfg(test)]
#[path = "dashboard_model_tests.rs"]
mod tests;
