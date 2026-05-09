//! Backend probes that resolve per-vendor quota and reset maps from the
//! provider adapters.
use crate::data::providers::{self, LiveModel};
use crate::logic::selection::types::{QuotaError, SubscriptionKind};
use crate::model_names;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
type VendorQuotaMap = BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>;
type VendorResetMap = BTreeMap<SubscriptionKind, BTreeMap<String, Option<DateTime<Utc>>>>;
type ModelQuotaMap = BTreeMap<String, Option<u8>>;
type ModelResetMap = BTreeMap<String, Option<DateTime<Utc>>>;
type ModelQuotaAndResetMaps = (ModelQuotaMap, ModelResetMap);
type QuotaLoadResult = (VendorQuotaMap, VendorResetMap, Vec<QuotaError>);
pub async fn load_quota_maps_for_async(
    vendors: impl IntoIterator<Item = SubscriptionKind>,
) -> QuotaLoadResult {
    let vendors = vendors.into_iter().collect::<Vec<_>>();
    let tasks = vendors
        .into_iter()
        .map(|vendor| {
            (
                vendor,
                tokio::spawn(async move { load_quota_map_for_vendor(vendor).await }),
            )
        })
        .collect::<Vec<_>>();
    let mut maps = BTreeMap::new();
    let mut reset_maps = BTreeMap::new();
    let mut errors = Vec::new();
    for (vendor, task) in tasks {
        let Ok(result) = task.await else {
            errors.push(QuotaError {
                vendor,
                message: "quota worker task failed".to_string(),
            });
            continue;
        };
        match result {
            Ok((map, reset_map)) => {
                maps.insert(vendor, map);
                reset_maps.insert(vendor, reset_map);
            }
            Err(e) => errors.push(QuotaError { vendor, message: e }),
        }
    }
    (maps, reset_maps, errors)
}
async fn load_quota_map_for_vendor(
    vendor: SubscriptionKind,
) -> Result<ModelQuotaAndResetMaps, String> {
    match vendor {
        SubscriptionKind::Codex => providers::codex::load_live_models_async()
            .await
            .map(live_map_codex)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Claude => providers::claude::load_live_models_async()
            .await
            .map(live_map_claude)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Gemini => providers::gemini::load_live_models_async()
            .await
            .map(live_map_direct)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Kimi => providers::kimi::load_live_models_async()
            .await
            .map(live_map_kimi)
            .map_err(|e| e.to_string()),
        SubscriptionKind::OpencodeGo => providers::opencode::load_live_models_async()
            .await
            .map(live_map_opencode)
            .map_err(|e| e.to_string()),
        SubscriptionKind::Direct => Ok((BTreeMap::new(), BTreeMap::new())),
    }
}
fn live_map_codex(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let raw = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect::<BTreeMap<_, _>>();
    let shared = raw
        .iter()
        .filter(|(name, _)| !name.contains("spark"))
        .find_map(|(_, quota)| *quota);
    let spark = raw
        .get("gpt-5.3-codex-spark")
        .copied()
        .flatten()
        .or_else(|| {
            raw.iter()
                .find(|(name, _)| name.contains("spark"))
                .and_then(|(_, quota)| *quota)
        });
    let mut mapped = BTreeMap::new();
    for name in raw.keys() {
        let quota = if name.contains("spark") {
            spark
        } else {
            shared
        };
        mapped.insert(name.clone(), quota);
    }
    for known_model in &[
        "gpt-5.3-codex",
        "gpt-5.3-codex-nova",
        "gpt-5.3-codex-terra",
        "gpt-5.3-codex-spark",
        "gpt-5.2-codex",
        "gpt-5-64k",
        "gpt-5",
        "gpt-4o-2025-01-20",
        "gpt-4o-latest",
    ] {
        let model_name = known_model.to_string();
        let has_spark = model_name.contains("spark");
        mapped
            .entry(model_name)
            .or_insert_with(|| if has_spark { spark } else { shared });
    }
    let resets = mapped.keys().map(|name| (name.clone(), None)).collect();
    (mapped, resets)
}
fn live_map_claude(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let raw = models
        .into_iter()
        .map(|model| {
            (
                model.name.to_ascii_lowercase(),
                (model.quota_percent, model.quota_resets_at),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let shared = raw
        .iter()
        .find(|(name, _)| {
            name.contains("sonnet") || name.contains("opus") || name.contains("haiku")
        })
        .and_then(|(_, (quota, _))| *quota)
        .or_else(|| raw.get("seven_day").and_then(|(quota, _)| *quota))
        .or_else(|| raw.get("five_hour").and_then(|(quota, _)| *quota))
        .or_else(|| raw.values().find_map(|(quota, _)| *quota));
    let shared_reset = raw.values().filter_map(|(_, reset)| *reset).min();
    let mut mapped = BTreeMap::new();
    let mut resets = BTreeMap::new();
    for name in raw.keys() {
        if name.starts_with("claude-") {
            mapped.insert(name.clone(), shared);
            resets.insert(name.clone(), shared_reset);
        }
    }
    for known_model in &[
        "claude-opus-4.7",
        "claude-opus-4.1",
        "claude-sonnet-4.6",
        "claude-sonnet-4-5-20250929",
        "claude-sonnet-3.5",
        "claude-haiku-4.5",
        "claude-haiku-3.5",
        "claude-3-opus",
        "claude-3-sonnet",
        "claude-3-haiku",
    ] {
        let model_name = known_model.to_string();
        mapped.entry(model_name).or_insert(shared);
        resets
            .entry(known_model.to_string())
            .or_insert(shared_reset);
    }
    (mapped, resets)
}
fn live_map_direct(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    let mut mapped: BTreeMap<String, Option<u8>> = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect();
    // Google's retrieveUserQuota only returns buckets it knows about, which
    // can lag new model names (e.g. gemini-3-flash). Inject known
    // names so dashboard::synthesize_sibling has something to extend a
    // sibling fallback onto.
    let shared = mapped.values().find_map(|q| *q);
    for known in model_names::GEMINI_KNOWN_QUOTA_MODELS {
        mapped.entry(known.to_string()).or_insert(shared);
    }
    let resets = mapped.keys().map(|name| (name.clone(), None)).collect();
    (mapped, resets)
}
fn live_map_opencode(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    // Opencode runs on a single Go-tier dollar pool, so any non-None entry
    // returned by the provider applies to every opencode-routed model name.
    // Surface a single shared key — baked entries point their
    // `quota_lookup_key` at it so per-row lookups resolve here.
    let quota = models.into_iter().find_map(|m| m.quota_percent);
    (
        BTreeMap::from([(providers::opencode::SHARED_QUOTA_KEY.to_string(), quota)]),
        BTreeMap::from([(providers::opencode::SHARED_QUOTA_KEY.to_string(), None)]),
    )
}
fn live_map_kimi(models: Vec<LiveModel>) -> ModelQuotaAndResetMaps {
    // Kimi runs every model off one shared usage pool, so we expose the
    // quota under a single sentinel key. Baked Kimi entries set
    // `quota_lookup_key = "kimi-shared"` so per-row lookups resolve here
    // without aliasing a real ipbr model id (the way the former
    // `kimi-latest` placeholder did).
    let quota = models.into_iter().filter_map(|m| m.quota_percent).min();
    (
        BTreeMap::from([(providers::kimi::SHARED_QUOTA_KEY.to_string(), quota)]),
        BTreeMap::from([(providers::kimi::SHARED_QUOTA_KEY.to_string(), None)]),
    )
}
#[cfg(test)]
#[path = "selection_quota_tests.rs"]
mod tests;
