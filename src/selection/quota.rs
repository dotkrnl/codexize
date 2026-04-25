use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread;
use crate::providers::{self, LiveModel};
use super::types::{VendorKind, QuotaError};

#[allow(clippy::type_complexity)]
pub fn load_quota_maps() -> (BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>, Vec<QuotaError>) {
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for vendor in [
            VendorKind::Codex,
            VendorKind::Claude,
            VendorKind::Gemini,
            VendorKind::Kimi,
        ] {
            let tx = tx.clone();
            scope.spawn(move || {
                let _ = tx.send((vendor, load_quota_map_for_vendor(vendor)));
            });
        }
        drop(tx);
        let mut maps = BTreeMap::new();
        let mut errors = Vec::new();
        for (vendor, result) in rx {
            match result {
                Ok(map) => { maps.insert(vendor, map); }
                Err(e) => errors.push(QuotaError { vendor, message: e }),
            }
        }
        (maps, errors)
    })
}

fn load_quota_map_for_vendor(vendor: VendorKind) -> Result<BTreeMap<String, Option<u8>>, String> {
    match vendor {
        VendorKind::Codex => providers::codex::load_live_models()
            .map(live_map_codex)
            .map_err(|e| e.to_string()),
        VendorKind::Claude => providers::claude::load_live_models()
            .map(live_map_claude)
            .map_err(|e| e.to_string()),
        VendorKind::Gemini => providers::gemini::load_live_models()
            .map(live_map_direct)
            .map_err(|e| e.to_string()),
        VendorKind::Kimi => providers::kimi::load_live_models()
            .map(live_map_kimi)
            .map_err(|e| e.to_string()),
    }
}

pub fn find_quota_by_heuristic(
    model_name: &str,
    vendor: VendorKind,
    quotas: &BTreeMap<VendorKind, BTreeMap<String, Option<u8>>>,
) -> Option<u8> {
    let vendor_quotas = quotas.get(&vendor)?;

    // For unknown models, try to find a similar model's quota
    match vendor {
        VendorKind::Codex => {
            // Check if it's a spark variant
            if model_name.contains("spark") || model_name.contains("mini") {
                vendor_quotas.iter()
                    .find(|(name, _)| name.contains("spark"))
                    .and_then(|(_, quota)| *quota)
            } else {
                // Use any non-spark model's quota as shared quota
                vendor_quotas.iter()
                    .find(|(name, _)| !name.contains("spark"))
                    .and_then(|(_, quota)| *quota)
            }
        }
        VendorKind::Claude => {
            // All Claude models typically share quota
            vendor_quotas.values().find_map(|q| *q)
        }
        VendorKind::Gemini => {
            // Check for pro vs flash variants
            if model_name.contains("flash") || model_name.contains("nano") {
                vendor_quotas.iter()
                    .find(|(name, _)| name.contains("flash") || name.contains("nano"))
                    .and_then(|(_, quota)| *quota)
            } else {
                vendor_quotas.iter()
                    .find(|(name, _)| name.contains("pro") || name.contains("ultra"))
                    .and_then(|(_, quota)| *quota)
                    .or_else(|| vendor_quotas.values().find_map(|q| *q))
            }
        }
        VendorKind::Kimi => {
            // All Kimi models typically share quota
            vendor_quotas.values().find_map(|q| *q)
        }
    }
}

fn live_map_codex(models: Vec<LiveModel>) -> BTreeMap<String, Option<u8>> {
    let raw = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect::<BTreeMap<_, _>>();

    // Find shared quota from any non-spark model that has quota
    let shared = raw
        .iter()
        .filter(|(name, _)| !name.contains("spark"))
        .find_map(|(_, quota)| *quota);

    // Find spark quota
    let spark = raw.get("gpt-5.3-codex-spark").copied().flatten()
        .or_else(|| raw.iter().find(|(name, _)| name.contains("spark")).and_then(|(_, quota)| *quota));

    // Map all known Codex models to appropriate quota
    let mut mapped = BTreeMap::new();

    // Add models we found in live probe
    for name in raw.keys() {
        let quota = if name.contains("spark") { spark } else { shared };
        mapped.insert(name.clone(), quota);
    }

    // Add additional known Codex models that might appear from dashboard
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
        mapped.entry(model_name).or_insert_with(|| {
            if has_spark { spark } else { shared }
        });
    }

    mapped
}

fn live_map_claude(models: Vec<LiveModel>) -> BTreeMap<String, Option<u8>> {
    let raw = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect::<BTreeMap<_, _>>();

    // Find shared quota from any Claude model or fallback keys
    let shared = raw
        .iter()
        .find(|(name, _)| name.contains("sonnet") || name.contains("opus") || name.contains("haiku"))
        .and_then(|(_, quota)| *quota)
        .or_else(|| raw.get("seven_day").copied().flatten())
        .or_else(|| raw.get("five_hour").copied().flatten())
        .or_else(|| raw.values().find_map(|q| *q));

    // Map all known Claude models to shared quota
    let mut mapped = BTreeMap::new();

    // Add models we found in live probe
    for name in raw.keys() {
        if name.starts_with("claude-") {
            mapped.insert(name.clone(), shared);
        }
    }

    // Add all known Claude models that might appear from dashboard
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
    }

    mapped
}

fn live_map_direct(models: Vec<LiveModel>) -> BTreeMap<String, Option<u8>> {
    let mut mapped: BTreeMap<String, Option<u8>> = models
        .into_iter()
        .map(|model| (model.name.to_ascii_lowercase(), model.quota_percent))
        .collect();

    // Google's retrieveUserQuota only returns buckets it knows about, which
    // can lag new model names (e.g. gemini-3.1-pro). Inject known names so
    // dashboard::synthesize_sibling has something to extend a sibling
    // fallback onto. Use the best observed quota as the shared default.
    let shared = mapped.values().find_map(|q| *q);
    for known in &[
        "gemini-3.1-pro",
        "gemini-3-pro-preview",
        "gemini-3-flash",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
    ] {
        mapped.entry((*known).to_string()).or_insert(shared);
    }

    mapped
}

fn live_map_kimi(models: Vec<LiveModel>) -> BTreeMap<String, Option<u8>> {
    // Kimi only has one effective model (kimi-latest); expose it under that
    // canonical name regardless of what the API returns.
    let quota = models
        .into_iter()
        .find_map(|m| m.quota_percent);
    BTreeMap::from([("kimi-latest".to_string(), quota)])
}
