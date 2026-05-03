use super::quota;
use super::ranking::stamp_selection_provenance;
use super::types::{CachedModel, QuotaError, VendorKind};
use super::vendor;
use crate::acp::AcpConfig;
use crate::cache::{self, DashboardEntry, LoadedCache, QuotaPayload, ResetPayload};
use crate::dashboard::{self, LoadOutcome};
use crate::selection::ScoreSource;
use std::cmp::Ordering;
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
            Ok(LoadOutcome::Both(fresh)) => {
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
            axis_provenance: BTreeMap::new(),
            ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
            score_source: crate::selection::ScoreSource::None,
            ipbr_row_matched: false,
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

    fn make_reset_payload(entries: &[(&str, &str, Option<&str>)]) -> ResetPayload {
        let mut payload: ResetPayload = BTreeMap::new();
        for (vendor, name, reset) in entries {
            payload.entry(vendor.to_string()).or_default().insert(
                name.to_string(),
                reset.map(|value| {
                    chrono::DateTime::parse_from_rfc3339(value)
                        .unwrap()
                        .with_timezone(&chrono::Utc)
                }),
            );
        }
        payload
    }

    fn empty_resets_for_quotas(quotas: &QuotaPayload) -> ResetPayload {
        quotas
            .iter()
            .map(|(vendor, models)| {
                (
                    vendor.clone(),
                    models.keys().map(|name| (name.clone(), None)).collect(),
                )
            })
            .collect()
    }

    fn loaded_cache_with(dashboard: Vec<DashboardEntry>, quotas: QuotaPayload) -> LoadedCache {
        let resets = empty_resets_for_quotas(&quotas);
        LoadedCache {
            dashboard: Some(cache::LoadedSection {
                data: dashboard,
                expired: false,
            }),
            quotas: Some(cache::LoadedSection {
                data: quotas,
                expired: false,
            }),
            quota_resets: Some(cache::LoadedSection {
                data: resets,
                expired: false,
            }),
        }
    }

    fn loaded_cache_with_resets(
        dashboard: Vec<DashboardEntry>,
        quotas: QuotaPayload,
        resets: ResetPayload,
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
            quota_resets: Some(cache::LoadedSection {
                data: resets,
                expired: false,
            }),
        }
    }

    #[test]
    fn assemble_merges_dashboard_and_quotas() {
        let mut claude_entry = make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0);
        claude_entry
            .axis_provenance
            .insert("correctness".to_string(), "suite:hourly".to_string());
        let dashboard = vec![claude_entry, make_entry("gpt-5.5", "openai", 80.0, 78.0)];
        let quotas = make_quota_payload(&[
            ("claude", "claude-sonnet-4-6", Some(80)),
            ("openai", "gpt-5.5", Some(70)),
        ]);

        let (models, errors) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert!(errors.is_empty());
        assert_eq!(models.len(), 2);
        let claude = models
            .iter()
            .find(|m| m.name == "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(claude.vendor, VendorKind::Claude);
        assert_eq!(claude.quota_percent, Some(80));
        assert_eq!(
            claude
                .axis_provenance
                .get("correctness")
                .map(String::as_str),
            Some("suite:hourly")
        );
        let codex = models.iter().find(|m| m.name == "gpt-5.5").unwrap();
        assert_eq!(codex.vendor, VendorKind::Codex);
        assert_eq!(codex.quota_percent, Some(70));
    }

    #[test]
    fn assemble_merges_cached_quota_resets() {
        let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
        let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
        let resets =
            make_reset_payload(&[("claude", "claude-sonnet-4-6", Some("2026-04-30T12:00:00Z"))]);

        let (models, errors) =
            assemble_from_cache(loaded_cache_with_resets(dashboard, quotas, resets));

        assert!(errors.is_empty());
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].quota_resets_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-04-30T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
    }

    #[test]
    fn assemble_refreshes_when_cached_reset_coverage_is_partial() {
        let dashboard = vec![
            make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
            make_entry("claude-opus-4-1", "claude", 84.0, 81.0),
        ];
        let quotas = make_quota_payload(&[
            ("claude", "claude-sonnet-4-6", Some(80)),
            ("claude", "claude-opus-4-1", Some(80)),
        ]);
        let resets = make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]);
        let available = BTreeSet::from([VendorKind::Claude]);
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let claude_path = bin_dir.join("claude");
        let security_path = bin_dir.join("security");
        std::fs::write(
            &claude_path,
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then\n  printf '{\"orgId\":\"test-org\"}'\n  exit 0\nfi\nsleep 1\n",
        )
        .unwrap();
        std::fs::write(&security_path, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&security_path, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let original_path = std::env::var_os("PATH");

        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            let mut paths = vec![bin_dir.clone()];
            if let Some(value) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&value));
            }
            let joined = std::env::join_paths(paths).unwrap();
            std::env::set_var("PATH", joined);
        }

        let (models, errors) = assemble_from_cache_with_available(
            loaded_cache_with_resets(dashboard, quotas, resets),
            &available,
        );

        unsafe {
            match original_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }

        assert_eq!(models.len(), 2);
        assert_eq!(errors.len(), 1, "partial reset gaps should trigger refresh");
        assert_eq!(errors[0].vendor, VendorKind::Claude);
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
    fn unavailable_vendors_are_omitted_before_models_are_returned() {
        let dashboard = vec![
            make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
            make_entry("gpt-5.5", "openai", 80.0, 78.0),
            make_entry("gemini-2.5-pro", "google", 75.0, 73.0),
        ];
        let quotas = make_quota_payload(&[
            ("claude", "claude-sonnet-4-6", Some(80)),
            ("openai", "gpt-5.5", Some(70)),
            ("google", "gemini-2.5-pro", Some(60)),
        ]);
        let available = BTreeSet::from([VendorKind::Codex]);

        let (models, errors) =
            assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

        assert!(errors.is_empty());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].vendor, VendorKind::Codex);
        assert_eq!(models[0].name, "gpt-5.5");
        assert_eq!(models[0].quota_percent, Some(70));
    }

    #[test]
    fn available_claude_keeps_anthropic_dashboard_entries() {
        let dashboard = vec![make_entry("claude-sonnet-4-6", "anthropic", 85.0, 82.0)];
        let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
        let available = BTreeSet::from([VendorKind::Claude]);

        let (models, errors) =
            assemble_from_cache_with_available(loaded_cache_with(dashboard, quotas), &available);

        assert!(errors.is_empty());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].vendor, VendorKind::Claude);
        assert_eq!(models[0].name, "claude-sonnet-4-6");
        assert_eq!(models[0].quota_percent, Some(80));
    }

    #[test]
    fn assemble_from_loaded_uses_acp_configured_vendor_availability() {
        let loaded = loaded_cache_with(
            vec![
                make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0),
                make_entry("gpt-5.5", "openai", 80.0, 78.0),
            ],
            make_quota_payload(&[
                ("claude", "claude-sonnet-4-6", Some(80)),
                ("openai", "gpt-5.5", Some(70)),
            ]),
        );
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_available = std::env::var_os("CODEXIZE_TEST_AVAILABLE_VENDORS");
        let original_claude = std::env::var_os("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM");
        let original_codex = std::env::var_os("CODEXIZE_TEST_ACP_CODEX_PROGRAM");
        let original_gemini = std::env::var_os("CODEXIZE_TEST_ACP_GEMINI_PROGRAM");
        let original_kimi = std::env::var_os("CODEXIZE_TEST_ACP_KIMI_PROGRAM");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            std::env::set_var("CODEXIZE_TEST_AVAILABLE_VENDORS", "claude");
            std::env::set_var(
                "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
                "/definitely/missing/claude",
            );
            std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", "/bin/sh");
            std::env::set_var(
                "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
                "/definitely/missing/gemini",
            );
            std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", "/definitely/missing/kimi");
        }

        let outcome = std::panic::catch_unwind(|| assemble_from_loaded(&loaded));

        unsafe {
            match original_available {
                Some(value) => std::env::set_var("CODEXIZE_TEST_AVAILABLE_VENDORS", value),
                None => std::env::remove_var("CODEXIZE_TEST_AVAILABLE_VENDORS"),
            }
            match original_claude {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM"),
            }
            match original_codex {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM"),
            }
            match original_gemini {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM"),
            }
            match original_kimi {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM"),
            }
        }

        let models = outcome.expect("assemble_from_loaded should not panic");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].vendor, VendorKind::Codex);
        assert_eq!(models[0].name, "gpt-5.5");
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
            quota_resets: Some(cache::LoadedSection {
                data: make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]),
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
            quota_resets: Some(cache::LoadedSection {
                data: make_reset_payload(&[("claude", "claude-sonnet", None)]),
                expired: false,
            }),
        };

        let (models, _) = assemble_from_cache(loaded);

        assert!(models.is_empty());
    }

    #[test]
    fn assemble_from_loaded_uses_provided_snapshot_without_reloading() {
        let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
        let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
        let available = [
            VendorKind::Codex,
            VendorKind::Claude,
            VendorKind::Gemini,
            VendorKind::Kimi,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        let models =
            assemble_from_loaded_with_available(&loaded_cache_with(dashboard, quotas), &available);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "claude-sonnet-4-6");
        assert_eq!(models[0].quota_percent, Some(80));
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
    fn merge_preserves_expired_vendor_on_error() {
        // Cached has data for all four vendors.
        let mut cached: QuotaPayload = BTreeMap::new();
        cached.insert(
            "claude".to_string(),
            BTreeMap::from([("claude-sonnet".to_string(), Some(50))]),
        );
        cached.insert(
            "openai".to_string(),
            BTreeMap::from([("gpt-5".to_string(), Some(60))]),
        );
        cached.insert(
            "google".to_string(),
            BTreeMap::from([("gemini-2.5-pro".to_string(), Some(70))]),
        );

        // Fresh refresh succeeded only for Claude.
        let mut fresh: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
        fresh.insert(
            VendorKind::Claude,
            BTreeMap::from([("claude-sonnet".to_string(), Some(80))]),
        );

        let merged = merge_quota_payload(&cached, fresh);

        // Claude was refreshed → fresh value wins.
        assert_eq!(
            merged
                .get("claude")
                .and_then(|m| m.get("claude-sonnet").copied()),
            Some(Some(80))
        );
        // OpenAI/Google failed to refresh → expired cached values preserved.
        assert_eq!(
            merged.get("openai").and_then(|m| m.get("gpt-5").copied()),
            Some(Some(60))
        );
        assert_eq!(
            merged
                .get("google")
                .and_then(|m| m.get("gemini-2.5-pro").copied()),
            Some(Some(70))
        );
    }

    #[test]
    fn merge_overlays_when_cached_uses_alias_key() {
        // Cached used the str_to_vendor alias ("codex") rather than vendor_kind_to_str ("openai").
        let mut cached: QuotaPayload = BTreeMap::new();
        cached.insert(
            "codex".to_string(),
            BTreeMap::from([("gpt-5".to_string(), Some(40))]),
        );

        let mut fresh: BTreeMap<VendorKind, BTreeMap<String, Option<u8>>> = BTreeMap::new();
        fresh.insert(
            VendorKind::Codex,
            BTreeMap::from([("gpt-5".to_string(), Some(90))]),
        );

        let merged = merge_quota_payload(&cached, fresh);

        // The alias entry is dropped (its vendor was refreshed) and the canonical
        // "openai" key carries the fresh value.
        assert!(!merged.contains_key("codex"));
        assert_eq!(
            merged.get("openai").and_then(|m| m.get("gpt-5").copied()),
            Some(Some(90))
        );
    }

    #[test]
    fn merge_keeps_unknown_vendor_keys() {
        let mut cached: QuotaPayload = BTreeMap::new();
        cached.insert(
            "aliens".to_string(),
            BTreeMap::from([("ufo-9000".to_string(), Some(33))]),
        );

        let merged = merge_quota_payload(&cached, BTreeMap::new());

        assert_eq!(
            merged
                .get("aliens")
                .and_then(|m| m.get("ufo-9000").copied()),
            Some(Some(33))
        );
    }

    #[test]
    fn missing_quota_results_in_none() {
        let dashboard = vec![make_entry("gemini-2.5-pro", "google", 85.0, 83.0)];
        let quotas = make_quota_payload(&[]);

        let (models, _) = assemble_from_cache(loaded_cache_with(dashboard, quotas));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].quota_percent, None);
    }

    #[test]
    fn reset_coverage_gaps_require_matching_model_keys() {
        let quotas = make_quota_payload(&[
            ("claude", "claude-sonnet-4-6", Some(80)),
            ("claude", "claude-opus-4-1", Some(80)),
        ]);
        let partial_resets = make_reset_payload(&[("claude", "claude-sonnet-4-6", None)]);
        let covered_resets = make_reset_payload(&[
            ("claude", "claude-sonnet-4-6", None),
            ("claude", "claude-opus-4-1", None),
        ]);

        assert!(has_reset_coverage_gaps(&quotas, &partial_resets));
        assert!(!has_reset_coverage_gaps(&quotas, &covered_resets));
    }

    fn with_temp_home_cache<T>(
        dashboard: Vec<DashboardEntry>,
        quotas: QuotaPayload,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let original = std::env::var_os("HOME");
        let original_claude = std::env::var_os("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM");
        let original_codex = std::env::var_os("CODEXIZE_TEST_ACP_CODEX_PROGRAM");
        let original_gemini = std::env::var_os("CODEXIZE_TEST_ACP_GEMINI_PROGRAM");
        let original_kimi = std::env::var_os("CODEXIZE_TEST_ACP_KIMI_PROGRAM");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            std::env::set_var("HOME", temp.path());
            std::env::set_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM", "/bin/sh");
            std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", "/bin/sh");
            std::env::set_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM", "/bin/sh");
            std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", "/bin/sh");
        }
        cache::save_dashboard(&dashboard).unwrap();
        cache::save_quotas(&quotas).unwrap();
        cache::save_quota_resets(&empty_resets_for_quotas(&quotas)).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match original {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match original_claude {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_CLAUDE_PROGRAM"),
            }
            match original_codex {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_CODEX_PROGRAM"),
            }
            match original_gemini {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_GEMINI_PROGRAM"),
            }
            match original_kimi {
                Some(value) => std::env::set_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM", value),
                None => std::env::remove_var("CODEXIZE_TEST_ACP_KIMI_PROGRAM"),
            }
        }
        outcome.unwrap()
    }

    #[test]
    fn assemble_models_uses_default_cache_dir_when_fresh() {
        let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
        let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
        with_temp_home_cache(dashboard, quotas, || {
            // Cache was just written, so dashboard + quotas are fresh; the
            // public wrapper should not need any network refresh.
            let (models, errors) = assemble_models();
            assert!(
                errors.is_empty(),
                "fresh cache should not trigger refresh errors"
            );
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].name, "claude-sonnet-4-6");
            assert_eq!(models[0].quota_percent, Some(80));
        });
    }

    #[test]
    fn assemble_from_cached_only_returns_empty_when_no_cache() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let original = std::env::var_os("HOME");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        let models = assemble_from_cached_only();
        unsafe {
            match original {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        assert!(models.is_empty(), "no cache should yield empty model list");
    }

    #[test]
    fn score_failure_prefers_cached_ipbr_entries_over_inventory_only() {
        let mut cached_with_ipbr = make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0);
        cached_with_ipbr.score_source = ScoreSource::Ipbr;
        cached_with_ipbr.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
            idea: Some(70.0),
            planning: Some(72.0),
            build: Some(73.0),
            review: Some(71.0),
        };
        let cached = vec![cached_with_ipbr];
        let inv_only = vec![make_entry("claude-sonnet-4-6", "claude", 0.0, 0.0)];

        let resolved = resolve_score_failure_entries(cached.clone(), inv_only);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].score_source, ScoreSource::Ipbr);
        assert_eq!(resolved[0].ipbr_phase_scores.build, Some(73.0));
    }

    #[test]
    fn score_failure_falls_back_to_inventory_only_when_no_cached_ipbr() {
        // No cached row carries `ScoreSource::Ipbr`, so the inventory-only
        // refresh must still surface so the strip is not blank — phase
        // scores stay `None` until ipbr recovers.
        let cached = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
        let inv_only = vec![make_entry("gpt-5.4", "openai", 0.0, 0.0)];

        let resolved = resolve_score_failure_entries(cached, inv_only);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "gpt-5.4");
        assert_eq!(resolved[0].score_source, ScoreSource::None);
    }

    #[test]
    fn score_failure_falls_back_to_inventory_only_when_cache_is_empty() {
        let inv_only = vec![make_entry("claude-sonnet-4-6", "claude", 0.0, 0.0)];

        let resolved = resolve_score_failure_entries(Vec::new(), inv_only);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "claude-sonnet-4-6");
    }

    #[test]
    fn assemble_from_cached_only_yields_models_when_cache_is_present() {
        let dashboard = vec![make_entry("claude-sonnet-4-6", "claude", 85.0, 82.0)];
        let quotas = make_quota_payload(&[("claude", "claude-sonnet-4-6", Some(80))]);
        with_temp_home_cache(dashboard, quotas, || {
            let models = assemble_from_cached_only();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].name, "claude-sonnet-4-6");
            assert_eq!(models[0].quota_percent, Some(80));
        });
    }
}
