#[cfg(test)]
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::dashboard::DashboardModel;
#[cfg(test)]
use crate::dashboard::IngestEvent;
use crate::model_names;
use crate::selection::{IpbrPhaseScores, ScoreSource};

#[derive(Debug, Clone)]
pub(crate) struct InventoryEntry {
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) display_order: usize,
}

/// Canonicalized score record produced by score ingestion. The `name`
/// field uses inventory-compatible `trim().to_ascii_lowercase()` shape so
/// the existing exact-match merge keeps working; richer normalization is
/// exposed via `canonical_id` / `aliases` for the upcoming matching task.
/// For ipbr-sourced rows, `score_source = Ipbr` and `ipbr_row_matched = true`;
/// the legacy aistupidlevel parser (kept only for tests) leaves
/// `score_source = None` so the cosmetic `axes` cannot masquerade as
/// ipbr authority.
#[derive(Debug, Clone)]
pub(crate) struct ScoreEntry {
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) overall_score: f64,
    pub(crate) current_score: f64,
    pub(crate) standard_error: f64,
    pub(crate) axes: Vec<(String, f64)>,
    pub(crate) axis_provenance: BTreeMap<String, String>,
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
    let legacy_score_map: HashMap<String, &ScoreEntry> = scores
        .iter()
        .filter(|s| s.score_source != ScoreSource::Ipbr)
        .map(|s| (s.name.clone(), s))
        .collect();

    let mut models: Vec<DashboardModel> = inventory
        .into_iter()
        .map(|inv| {
            if let Some(sc) = ipbr_lookup.matches.get(&normalize_ipbr_key(&inv.name)) {
                dashboard_model_from_score(inv.name, &inv.vendor, sc, None)
            } else if let Some(sc) = legacy_score_map.get(&inv.name) {
                dashboard_model_from_score(inv.name, &inv.vendor, sc, None)
            } else if let Some(sc) = sibling_score(&inv.name, &scores) {
                dashboard_model_from_score(inv.name, &inv.vendor, sc, Some(sc.name.clone()))
            } else {
                DashboardModel {
                    name: inv.name,
                    vendor: inv.vendor,
                    overall_score: 0.0,
                    current_score: 0.0,
                    standard_error: 0.0,
                    axes: Vec::new(),
                    axis_provenance: BTreeMap::new(),
                    ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
                    score_source: crate::selection::ScoreSource::None,
                    ipbr_row_matched: false,
                    display_order: inv.display_order + 10_000,
                    fallback_from: None,
                }
            }
        })
        .collect();

    let inv_names: std::collections::HashSet<String> =
        models.iter().map(|m| m.name.clone()).collect();
    for sc in &scores {
        if !inv_names.contains(&sc.name) {
            models.push(dashboard_model_from_score(
                sc.name.clone(),
                &sc.vendor,
                sc,
                None,
            ));
        }
    }

    models.sort_by_key(|m| m.display_order);
    MergeResult {
        models,
        warnings: ipbr_lookup.warnings,
    }
}

pub(crate) fn scores_only(scores: Vec<ScoreEntry>) -> Vec<DashboardModel> {
    scores
        .into_iter()
        .map(|sc| DashboardModel {
            name: sc.name,
            vendor: sc.vendor,
            overall_score: sc.overall_score,
            current_score: sc.current_score,
            standard_error: sc.standard_error,
            axes: sc.axes,
            axis_provenance: sc.axis_provenance,
            ipbr_phase_scores: sc.ipbr_phase_scores,
            score_source: sc.score_source,
            ipbr_row_matched: sc.ipbr_row_matched,
            display_order: sc.display_order,
            fallback_from: None,
        })
        .collect()
}

pub(crate) fn inv_only(inventory: Vec<InventoryEntry>) -> Vec<DashboardModel> {
    inventory
        .into_iter()
        .map(|inv| DashboardModel {
            name: inv.name,
            vendor: inv.vendor,
            overall_score: 0.0,
            current_score: 0.0,
            standard_error: 0.0,
            axes: Vec::new(),
            axis_provenance: BTreeMap::new(),
            ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
            score_source: crate::selection::ScoreSource::None,
            ipbr_row_matched: false,
            display_order: inv.display_order,
            fallback_from: None,
        })
        .collect()
}

pub fn synthesize_sibling(
    name: &str,
    vendor: &str,
    existing: &[DashboardModel],
) -> Option<DashboardModel> {
    let sibling = explicit_fallback(name, existing).or_else(|| {
        let stem = version_stem(name)?;
        existing
            .iter()
            .filter(|m| m.name != name && version_stem(&m.name) == Some(stem))
            .max_by(|a, b| {
                a.overall_score
                    .partial_cmp(&b.overall_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    })?;

    Some(DashboardModel {
        name: name.to_string(),
        vendor: if !vendor.is_empty() {
            vendor.to_string()
        } else {
            sibling.vendor.clone()
        },
        overall_score: sibling.overall_score,
        current_score: sibling.current_score,
        standard_error: sibling.standard_error,
        axes: sibling.axes.clone(),
        axis_provenance: sibling.axis_provenance.clone(),
        // Sibling-synthesized models inherit cosmetic display state but
        // MUST NOT inherit ipbr authority — only an explicit ipbr row
        // match may set those fields. See spec "Model Matching".
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        display_order: sibling.display_order,
        fallback_from: Some(sibling.name.clone()),
    })
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) fn merged_axes(
    value: &Value,
) -> Option<(
    Vec<(String, f64)>,
    BTreeMap<String, String>,
    Vec<IngestEvent>,
)> {
    let entries = value.as_array()?;
    let mut axes: BTreeMap<String, f64> = BTreeMap::new();
    let mut provenance: BTreeMap<String, String> = BTreeMap::new();
    let mut events = Vec::new();

    for entry in entries.iter().rev() {
        let suite = entry
            .get("suite")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(entry_axes) = entry.get("axes").and_then(Value::as_object) else {
            continue;
        };
        for (k, v) in entry_axes {
            let key = k.to_ascii_lowercase();
            if key == "contextwindow" {
                provenance
                    .entry(key)
                    .or_insert_with(|| "dropped:contextwindow".to_string());
                events.push(IngestEvent::AxisDropped {
                    reason: "contextwindow".to_string(),
                });
                continue;
            }
            if axes.contains_key(&key) {
                continue;
            }
            match value_to_f64(Some(v)) {
                Some(num) => {
                    axes.insert(key.clone(), num);
                    provenance.insert(key, format!("suite:{suite}"));
                }
                None => {
                    events.push(IngestEvent::AxisParseFail {
                        suite: suite.clone(),
                        axis: key,
                    });
                }
            }
        }
    }

    Some((axes.into_iter().collect(), provenance, events))
}

#[cfg(test)]
pub(crate) fn value_to_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn dashboard_model_from_score(
    name: String,
    inventory_vendor: &str,
    sc: &ScoreEntry,
    fallback_from: Option<String>,
) -> DashboardModel {
    let is_sibling_fallback = fallback_from.is_some();
    DashboardModel {
        name,
        vendor: if !inventory_vendor.is_empty() {
            inventory_vendor.to_string()
        } else {
            sc.vendor.clone()
        },
        overall_score: sc.overall_score,
        current_score: sc.current_score,
        standard_error: sc.standard_error,
        axes: sc.axes.clone(),
        axis_provenance: sc.axis_provenance.clone(),
        // ipbr-sourced rows propagate phase scores and `Ipbr` provenance;
        // legacy aistupidlevel-sourced rows leave `score_source = None`
        // so cosmetic `axes` cannot masquerade as ipbr authority.
        ipbr_phase_scores: if is_sibling_fallback {
            IpbrPhaseScores::default()
        } else {
            sc.ipbr_phase_scores
        },
        score_source: if is_sibling_fallback {
            ScoreSource::None
        } else {
            sc.score_source
        },
        ipbr_row_matched: !is_sibling_fallback && sc.ipbr_row_matched,
        display_order: sc.display_order,
        fallback_from,
    }
}

fn version_stem(name: &str) -> Option<&str> {
    let (prefix, tail) = name.rsplit_once('.')?;
    if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
        Some(prefix)
    } else {
        None
    }
}

fn sibling_score<'a>(name: &str, scores: &'a [ScoreEntry]) -> Option<&'a ScoreEntry> {
    let stem = version_stem(name)?;
    scores
        .iter()
        .filter(|sc| sc.name != name && version_stem(&sc.name) == Some(stem))
        .max_by(|a, b| {
            a.overall_score
                .partial_cmp(&b.overall_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

struct IpbrLookup<'a> {
    matches: HashMap<String, &'a ScoreEntry>,
    warnings: Vec<String>,
}

fn build_ipbr_lookup(scores: &[ScoreEntry]) -> IpbrLookup<'_> {
    let mut owners: HashMap<String, usize> = HashMap::new();
    let mut collisions: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();

    for (index, score) in scores.iter().enumerate() {
        if score.score_source != ScoreSource::Ipbr {
            continue;
        }

        for key in ipbr_keys_for_score(score) {
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

    let matches = owners
        .into_iter()
        .map(|(key, index)| (key, &scores[index]))
        .collect();
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

    IpbrLookup { matches, warnings }
}

fn ipbr_keys_for_score(score: &ScoreEntry) -> BTreeSet<String> {
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
    for alias in &score.aliases {
        let key = normalize_ipbr_key(alias);
        if !key.is_empty() {
            keys.insert(key);
        }
    }
    keys
}

fn explicit_fallback<'a>(name: &str, existing: &'a [DashboardModel]) -> Option<&'a DashboardModel> {
    let target = model_names::EXPLICIT_SCORE_FALLBACKS
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)?;
    existing.iter().find(|m| m.name == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(name: &str, value: f64, order: usize) -> ScoreEntry {
        ScoreEntry {
            name: name.to_string(),
            vendor: "vendor".to_string(),
            overall_score: value,
            current_score: value,
            standard_error: 0.0,
            axes: vec![("correctness".to_string(), value)],
            axis_provenance: BTreeMap::new(),
            display_order: order,
            canonical_id: None,
            aliases: Vec::new(),
            ipbr_phase_scores: IpbrPhaseScores::default(),
            score_source: ScoreSource::None,
            ipbr_row_matched: false,
        }
    }

    fn ipbr_score(
        name: &str,
        canonical_id: Option<&str>,
        aliases: &[&str],
        value: f64,
        order: usize,
    ) -> ScoreEntry {
        ScoreEntry {
            canonical_id: canonical_id.map(normalize_ipbr_key),
            aliases: aliases
                .iter()
                .map(|alias| normalize_ipbr_key(alias))
                .collect(),
            ipbr_phase_scores: IpbrPhaseScores {
                idea: Some(value),
                planning: Some(value + 1.0),
                build: Some(value + 2.0),
                review: Some(value + 3.0),
            },
            score_source: ScoreSource::Ipbr,
            ipbr_row_matched: true,
            ..score(name, value, order)
        }
    }

    fn inventory(name: &str, order: usize) -> InventoryEntry {
        InventoryEntry {
            name: name.to_string(),
            vendor: String::new(),
            display_order: order,
        }
    }

    #[test]
    fn merge_enriches_inventory_and_preserves_unscored_models() {
        let models = merge(
            vec![inventory("gpt-5.5", 0), inventory("claude-sonnet-4.5", 1)],
            vec![score("gpt-5.4", 0.8, 0)],
        );
        let synthesized = models
            .iter()
            .find(|model| model.name == "gpt-5.5")
            .expect("inventory sibling should be retained");
        let unscored = models
            .iter()
            .find(|model| model.name == "claude-sonnet-4.5")
            .expect("unscored inventory model should be retained");

        assert_eq!(synthesized.fallback_from.as_deref(), Some("gpt-5.4"));
        assert_eq!(unscored.overall_score, 0.0);
    }

    #[test]
    fn merge_matches_inventory_by_normalized_ipbr_aliases() {
        let models = merge(
            vec![inventory("claude-opus-4.1", 0)],
            vec![ipbr_score(
                "Claude Opus 4",
                None,
                &["claude_opus_4_1"],
                91.0,
                3,
            )],
        );

        let model = models
            .iter()
            .find(|model| model.name == "claude-opus-4.1")
            .expect("inventory model should remain visible");

        assert_eq!(model.score_source, ScoreSource::Ipbr);
        assert_eq!(model.ipbr_phase_scores.build, Some(93.0));
        assert_eq!(model.fallback_from, None);
    }

    #[test]
    fn merge_matches_inventory_by_normalized_provider_path_aliases() {
        let models = merge(
            vec![inventory("anthropic/claude-opus-4", 0)],
            vec![ipbr_score(
                "Claude Opus 4",
                Some("anthropic/claude-opus-4"),
                &[],
                88.0,
                2,
            )],
        );

        let model = models
            .iter()
            .find(|model| model.name == "anthropic/claude-opus-4")
            .expect("provider-path inventory model should remain visible");

        assert_eq!(model.score_source, ScoreSource::Ipbr);
        assert_eq!(model.ipbr_phase_scores.review, Some(91.0));
        assert_eq!(model.fallback_from, None);
    }

    #[test]
    fn merge_excludes_collided_normalized_ipbr_keys_and_warns() {
        let result = merge_with_warnings(
            vec![inventory("claude-opus-4.1", 0)],
            vec![
                ipbr_score("Claude Opus 4.1", None, &[], 90.0, 1),
                ipbr_score("Other Opus", None, &["claude_opus_4_1"], 70.0, 2),
            ],
        );

        let model = result
            .models
            .iter()
            .find(|model| model.name == "claude-opus-4.1")
            .expect("inventory model should remain visible after collision");

        assert_eq!(model.score_source, ScoreSource::None);
        assert_eq!(model.ipbr_phase_scores, IpbrPhaseScores::default());
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("claude-opus-4-1"));
    }

    #[test]
    fn merge_keeps_unmatched_inventory_model_visible_and_unscored() {
        let models = merge(
            vec![inventory("unlisted-model", 0)],
            vec![ipbr_score("Claude Opus 4", None, &[], 86.0, 1)],
        );

        let model = models
            .iter()
            .find(|model| model.name == "unlisted-model")
            .expect("inventory-only model should remain visible");

        assert_eq!(model.score_source, ScoreSource::None);
        assert_eq!(model.ipbr_phase_scores, IpbrPhaseScores::default());
        assert!(!model.ipbr_row_matched);
    }

    #[test]
    fn merge_marks_ipbr_sibling_synthesis_as_cosmetic_only() {
        let models = merge(
            vec![inventory("gpt-5.5", 0)],
            vec![ipbr_score("gpt-5.4", None, &[], 86.0, 1)],
        );

        let model = models
            .iter()
            .find(|model| model.name == "gpt-5.5")
            .expect("sibling synthesized model should remain visible");

        assert_eq!(model.fallback_from.as_deref(), Some("gpt-5.4"));
        assert_eq!(model.overall_score, 86.0);
        assert_eq!(model.score_source, ScoreSource::None);
        assert_eq!(model.ipbr_phase_scores, IpbrPhaseScores::default());
        assert!(!model.ipbr_row_matched);
    }

    #[test]
    fn scores_only_converts_scores_without_fallback() {
        let models = scores_only(vec![score("gpt-5.4", 0.8, 2)]);

        assert_eq!(models[0].name, "gpt-5.4");
        assert_eq!(models[0].fallback_from, None);
    }

    #[test]
    fn inv_only_zeroes_scores() {
        let models = inv_only(vec![inventory("gpt-5.5", 1)]);

        assert_eq!(models[0].overall_score, 0.0);
        assert_eq!(models[0].display_order, 1);
    }

    #[test]
    fn synthesize_sibling_uses_same_stem_score() {
        let existing = scores_only(vec![score("gpt-5.4", 0.8, 0)]);
        let synthesized = synthesize_sibling("gpt-5.5", "", &existing).unwrap();

        assert_eq!(synthesized.fallback_from.as_deref(), Some("gpt-5.4"));
        assert_eq!(synthesized.overall_score, 0.8);
    }

    #[test]
    fn merged_axes_returns_axes_provenance_and_events() {
        let val = serde_json::json!([
            {
                "suite": "deep",
                "axes": {
                    "contextWindow": 1.0,
                    "correctness": "bad"
                }
            },
            {
                "suite": "tooling",
                "axes": { "efficiency": "0.42" }
            }
        ]);
        let (axes, provenance, events) = merged_axes(&val).unwrap();

        assert_eq!(axes, vec![("efficiency".to_string(), 0.42)]);
        assert_eq!(
            provenance.get("contextwindow").map(String::as_str),
            Some("dropped:contextwindow")
        );
        assert!(events.iter().any(|event| matches!(
            event,
            IngestEvent::AxisDropped { reason } if reason == "contextwindow"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            IngestEvent::AxisParseFail { suite, axis }
                if suite == "deep" && axis == "correctness"
        )));
    }

    #[test]
    fn value_to_f64_accepts_numbers_strings_and_bools() {
        assert_eq!(value_to_f64(Some(&serde_json::json!(0.25))), Some(0.25));
        assert_eq!(value_to_f64(Some(&serde_json::json!("0.5"))), Some(0.5));
        assert_eq!(value_to_f64(Some(&serde_json::json!(true))), Some(1.0));
    }

    #[test]
    fn value_to_string_preserves_strings_and_serializes_other_values() {
        assert_eq!(value_to_string(&serde_json::json!("abc")), "abc");
        assert_eq!(value_to_string(&serde_json::json!(7)), "7");
    }
}
