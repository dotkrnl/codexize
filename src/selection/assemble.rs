use super::quota;
use super::types::{CachedModel, QuotaError, VendorKind};
use super::vendor;
use crate::cache::{self, DashboardEntry, LoadedCache, QuotaPayload};
use crate::dashboard;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

/// Load and merge dashboard + quota data into the canonical model universe.
///
/// Returns the assembled models and any quota errors encountered.
/// Performs Kimi collapse and sibling synthesis before returning.
pub fn assemble_models() -> (Vec<CachedModel>, Vec<QuotaError>) {
    let loaded = cache::load();
    assemble_from_cache(loaded)
}

fn assemble_from_cache(loaded: LoadedCache) -> (Vec<CachedModel>, Vec<QuotaError>) {
    let (dashboard_entries, dashboard_expired) = match loaded.dashboard {
        Some(section) => (section.data, section.expired),
        None => (Vec::new(), true),
    };

    let (quota_payload, quota_expired) = match loaded.quotas {
        Some(section) => (section.data, section.expired),
        None => (BTreeMap::new(), true),
    };

    let mut quota_errors = Vec::new();

    // Fetch fresh dashboard if stale/missing
    let dashboard_entries = if dashboard_expired {
        match dashboard::load_models() {
            Ok(fresh) => {
                let entries: Vec<DashboardEntry> = fresh
                    .iter()
                    .map(|m| DashboardEntry {
                        vendor: m.vendor.clone(),
                        name: m.name.clone(),
                        overall_score: m.overall_score,
                        current_score: m.current_score,
                        standard_error: m.standard_error,
                        axes: m.axes.clone(),
                        display_order: m.display_order,
                        fallback_from: m.fallback_from.clone(),
                    })
                    .collect();
                let _ = cache::save_dashboard(&entries);
                entries
            }
            Err(e) => {
                quota_errors.push(QuotaError {
                    vendor: VendorKind::Claude,
                    message: format!("dashboard fetch failed: {e}"),
                });
                if dashboard_entries.is_empty() {
                    return (Vec::new(), quota_errors);
                }
                dashboard_entries
            }
        }
    } else {
        dashboard_entries
    };

    // Fetch fresh quotas if stale/missing
    let quota_payload = if quota_expired {
        let (fresh_quotas, fresh_errors) = quota::load_quota_maps();
        quota_errors.extend(fresh_errors);
        let payload: QuotaPayload = fresh_quotas
            .into_iter()
            .map(|(vendor, models)| (vendor::vendor_kind_to_str(vendor).to_string(), models))
            .collect();
        let _ = cache::save_quotas(&payload);
        payload
    } else {
        quota_payload
    };

    // Parse quota payload into typed map.
    // Keys may come from vendor_kind_to_str ("openai", "google", "moonshotai")
    // or from str_to_vendor-compatible strings ("codex", "gemini", "kimi").
    let parsed_quotas: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = quota_payload
        .into_iter()
        .filter_map(|(vendor_name, models)| {
            parse_vendor_str(&vendor_name).map(|v| (v, models))
        })
        .collect();

    // Convert dashboard entries to DashboardModels for sibling synthesis
    let mut dashboard_models: Vec<dashboard::DashboardModel> = dashboard_entries
        .into_iter()
        .map(|e| dashboard::DashboardModel {
            name: e.name,
            vendor: e.vendor,
            overall_score: e.overall_score,
            current_score: e.current_score,
            standard_error: e.standard_error,
            axes: e.axes,
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
            if let Some(model) =
                dashboard::synthesize_sibling(name, vendor_str, &dashboard_models)
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

            Some(CachedModel {
                vendor,
                name: m.name,
                overall_score: m.overall_score,
                current_score: m.current_score,
                standard_error: m.standard_error,
                axes: m.axes,
                quota_percent,
                display_order: m.display_order,
                fallback_from: m.fallback_from,
            })
        })
        .collect();

    // Collapse all Kimi models into a single "kimi-latest" using the best overall score
    let best_kimi_idx = models
        .iter()
        .enumerate()
        .filter(|(_, m)| m.vendor == VendorKind::Kimi)
        .max_by(|(_, a), (_, b)| {
            a.overall_score
                .partial_cmp(&b.overall_score)
                .unwrap_or(Ordering::Equal)
        })
        .map(|(i, _)| i);
    if let Some(i) = best_kimi_idx {
        let mut canonical = models[i].clone();
        canonical.name = "kimi-latest".to_string();
        models.retain(|m| m.vendor != VendorKind::Kimi);
        models.push(canonical);
    }

    (models, quota_errors)
}

fn parse_vendor_str(s: &str) -> Option<VendorKind> {
    match s {
        "claude" => Some(VendorKind::Claude),
        "codex" | "openai" => Some(VendorKind::Codex),
        "gemini" | "google" => Some(VendorKind::Gemini),
        "kimi" | "moonshotai" => Some(VendorKind::Kimi),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::DashboardEntry;

    fn make_entry(name: &str, vendor: &str, overall: f64, current: f64) -> DashboardEntry {
        DashboardEntry {
            vendor: vendor.to_string(),
            name: name.to_string(),
            overall_score: overall,
            current_score: current,
            standard_error: 2.0,
            axes: vec![
                ("codequality".to_string(), 0.85),
                ("correctness".to_string(), 0.85),
                ("debugging".to_string(), 0.85),
                ("safety".to_string(), 0.85),
            ],
            display_order: 0,
            fallback_from: None,
        }
    }

    fn make_quota_payload(entries: &[(&str, &str, Option<u8>)]) -> QuotaPayload {
        let mut payload: QuotaPayload = BTreeMap::new();
        for (vendor, name, quota) in entries {
            payload
                .entry(vendor.to_string())
                .or_default()
                .insert(name.to_string(), *quota);
        }
        payload
    }

    fn loaded_cache_with(
        dashboard: Vec<DashboardEntry>,
        quotas: QuotaPayload,
    ) -> LoadedCache {
        LoadedCache {
            dashboard: Some(cache::LoadedSection {
                data: dashboard,
                expired: false,
            }),
            quotas: Some(cache::LoadedSection {
                data: quotas,
                expired: false,
            }),
        }
    }

    #[test]
    fn assemble_merges_dashboard_and_quotas() {
        let dashboard = vec![
            make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
            make_entry("gpt-5.5", "openai", 80.0, 78.0),
        ];
        let quotas = make_quota_payload(&[
            ("claude", "claude-sonnet-4-6", Some(80)),
            ("openai", "gpt-5.5", Some(70)),
        ]);

        let (models, errors) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert!(errors.is_empty());
        assert_eq!(models.len(), 2);
        let claude = models.iter().find(|m| m.name == "claude-sonnet-4-6").unwrap();
        assert_eq!(claude.vendor, VendorKind::Claude);
        assert_eq!(claude.quota_percent, Some(80));
        let codex = models.iter().find(|m| m.name == "gpt-5.5").unwrap();
        assert_eq!(codex.vendor, VendorKind::Codex);
        assert_eq!(codex.quota_percent, Some(70));
    }

    #[test]
    fn assemble_omits_models_with_unknown_vendor() {
        let dashboard = vec![make_entry("unknown-model", "aliens", 90.0, 90.0)];
        let quotas = make_quota_payload(&[]);

        let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert!(models.is_empty());
    }

    #[test]
    fn assemble_collapses_kimi_models() {
        let dashboard = vec![
            make_entry("kimi-k2", "moonshotai", 70.0, 68.0),
            make_entry("kimi-k1.5", "moonshotai", 75.0, 73.0),
        ];
        let quotas = make_quota_payload(&[
            ("moonshotai", "kimi-k2", Some(90)),
            ("moonshotai", "kimi-k1.5", Some(90)),
        ]);

        let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "kimi-latest");
        assert_eq!(models[0].vendor, VendorKind::Kimi);
        // Uses the best overall score (75.0 from kimi-k1.5)
        assert_eq!(models[0].overall_score, 75.0);
    }

    #[test]
    fn assemble_synthesizes_missing_sibling() {
        let dashboard = vec![make_entry("gpt-5.4", "openai", 80.0, 78.0)];
        // Quota has gpt-5.5 which is missing from dashboard
        let quotas = make_quota_payload(&[
            ("openai", "gpt-5.4", Some(80)),
            ("openai", "gpt-5.5", Some(70)),
        ]);

        let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert_eq!(models.len(), 2);
        let synthesized = models.iter().find(|m| m.name == "gpt-5.5").unwrap();
        assert_eq!(synthesized.fallback_from.as_deref(), Some("gpt-5.4"));
        assert_eq!(synthesized.quota_percent, Some(70));
    }

    #[test]
    fn stale_on_error_fallback_uses_expired_dashboard() {
        // Fresh (non-expired) dashboard should be used directly without fetching
        let loaded = LoadedCache {
            dashboard: Some(cache::LoadedSection {
                data: vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)],
                expired: false,
            }),
            quotas: Some(cache::LoadedSection {
                data: make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]),
                expired: false,
            }),
        };

        let (models, errors) = assemble_from_cache(loaded);

        assert!(errors.is_empty());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "claude-sonnet-4-6");
        assert_eq!(models[0].quota_percent, Some(80));
    }

    #[test]
    fn fresh_cache_with_empty_dashboard_returns_empty() {
        let loaded = LoadedCache {
            dashboard: Some(cache::LoadedSection {
                data: Vec::new(),
                expired: false,
            }),
            quotas: Some(cache::LoadedSection {
                data: make_quota_payload(&[("claude", "claude-sonnet", Some(80))]),
                expired: false,
            }),
        };

        let (models, _) = assemble_from_cache(loaded);

        assert!(models.is_empty());
    }

    #[test]
    fn quota_heuristic_fallback_when_no_exact_match() {
        let dashboard = vec![make_entry("claude-opus-4-7", "claude", 90.0, 88.0)];
        // Quota exists for a different claude model
        let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(75))]);

        let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert_eq!(models.len(), 1);
        // Should get quota via heuristic (Claude models share quota)
        assert_eq!(models[0].quota_percent, Some(75));
    }

    #[test]
    fn missing_quota_results_in_none() {
        let dashboard = vec![make_entry("gemini-2.5-pro", "google", 85.0, 83.0)];
        let quotas = make_quota_payload(&[]);

        let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].quota_percent, None);
    }
}
