//! Pure quota heuristics shared between the logic merge code and any
//! IO-performing loader. These helpers take an already-resolved quota or
//! reset map and pick the best fallback for a given model name; they
//! perform no backend IO and so are safe to call from the logic layer.
use super::types::SubscriptionKind;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
pub fn find_quota_by_heuristic(
    model_name: &str,
    vendor: SubscriptionKind,
    quotas: &BTreeMap<SubscriptionKind, BTreeMap<String, Option<u8>>>,
) -> Option<u8> {
    let vendor_quotas = quotas.get(&vendor)?;
    match vendor {
        SubscriptionKind::Codex => {
            if model_name.contains("spark") || model_name.contains("mini") {
                vendor_quotas
                    .iter()
                    .find(|(name, _)| name.contains("spark"))
                    .and_then(|(_, quota)| *quota)
            } else {
                vendor_quotas
                    .iter()
                    .find(|(name, _)| !name.contains("spark"))
                    .and_then(|(_, quota)| *quota)
            }
        }
        SubscriptionKind::Claude => vendor_quotas.values().find_map(|q| *q),
        SubscriptionKind::Gemini => {
            if model_name.contains("flash") || model_name.contains("nano") {
                vendor_quotas
                    .iter()
                    .find(|(name, _)| name.contains("flash") || name.contains("nano"))
                    .and_then(|(_, quota)| *quota)
            } else {
                vendor_quotas
                    .iter()
                    .find(|(name, _)| name.contains("pro") || name.contains("ultra"))
                    .and_then(|(_, quota)| *quota)
                    .or_else(|| vendor_quotas.values().find_map(|q| *q))
            }
        }
        SubscriptionKind::Kimi | SubscriptionKind::OpencodeGo => {
            vendor_quotas.values().find_map(|q| *q)
        }
        SubscriptionKind::Free => Some(100),
    }
}
pub fn find_reset_by_heuristic(
    model_name: &str,
    vendor: SubscriptionKind,
    resets: &BTreeMap<SubscriptionKind, BTreeMap<String, Option<DateTime<Utc>>>>,
) -> Option<DateTime<Utc>> {
    let vendor_resets = resets.get(&vendor)?;
    match vendor {
        SubscriptionKind::Claude => vendor_resets
            .get(model_name)
            .copied()
            .flatten()
            .or_else(|| vendor_resets.values().find_map(|reset| *reset)),
        _ => vendor_resets.get(model_name).copied().flatten(),
    }
}
