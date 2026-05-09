//! Baked default provider table for known dashboard `(vendor, model)`
//! pairs. The table seeds every provider's per-tuple knobs (eligibility,
//! effort mapping, official/free flags) so the operator can override
//! individual fields from TOML without losing the rest of the baked
//! defaults.
//!
//! Resolution rules (per spec §"Migration and Conflicts"):
//! - User entries with the same `(vendor, model, cli, launch_name)` as a
//!   baked entry **override** the baked properties field-by-field where
//!   the user explicitly diverged.
//! - User entries with a tuple not in the baked table are **additions**
//!   (new providers, `display_order = u16::MAX`).
//! - Baked entries cannot be removed; setting `enabled = false` is the
//!   only way to take one out of selection.
//!
//! There is no runtime name-heuristic fallback: a model that isn't in
//! the baked table and has no user provider has zero candidates.

use crate::data::config::schema::{EffortMapping, ProviderEntry};
use crate::selection::CliKind;

/// One row in the baked defaults table — a `(vendor, model)` pair plus
/// its ordered list of baked providers. The ordering of `providers`
/// drives the seeded `display_order`.
pub struct BakedRow {
    pub vendor: &'static str,
    pub model: &'static str,
    pub providers: &'static [BakedProvider],
}

/// One baked provider entry. Identity is `(cli, launch_name)`; the
/// remaining fields are the seeded baked defaults the resolver hands
/// out when no user entry overrides them.
pub struct BakedProvider {
    pub cli: CliKind,
    pub launch_name: &'static str,
    pub free: bool,
    pub official: bool,
    pub cheap_eligible: bool,
    pub tough_eligible: bool,
    pub effort_eligible: bool,
    pub effort_cheap: &'static str,
    pub effort_normal: &'static str,
    pub effort_tough: &'static str,
}

/// Sentinel display order for user-additions with no baked counterpart.
pub const ADDITION_DISPLAY_ORDER: u16 = u16::MAX;

/// Static baked-defaults table. The list is intentionally short — only
/// the tuples we actively ship — and grows as new vendor/model rows
/// land on the dashboard. New entries should mirror the heuristic seeds
/// used by the legacy assembly path so no operator-facing eligibility
/// flips silently when this table replaces them.
pub const BAKED_TABLE: &[BakedRow] = &[
    BakedRow {
        vendor: "claude",
        model: "claude-opus-4-7",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-opus-4-7",
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            // Spec: "Codex is xhigh, claude is max, though."
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
        }],
    },
    BakedRow {
        vendor: "claude",
        model: "claude-sonnet-4-6",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-sonnet-4-6",
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
        }],
    },
    BakedRow {
        vendor: "codex",
        model: "gpt-5",
        providers: &[BakedProvider {
            cli: CliKind::Codex,
            launch_name: "gpt-5",
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            // Spec: "Codex is xhigh".
            effort_tough: "xhigh",
        }],
    },
    BakedRow {
        vendor: "gemini",
        model: "gemini-2.5-pro",
        providers: &[BakedProvider {
            cli: CliKind::Gemini,
            launch_name: "gemini-2.5-pro",
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
        }],
    },
    BakedRow {
        vendor: "kimi",
        model: "kimi-latest",
        providers: &[BakedProvider {
            cli: CliKind::Kimi,
            launch_name: "kimi-latest",
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
        }],
    },
];

/// Materialize a baked provider as a concrete [`ProviderEntry`]. The
/// resolver below uses this when a baked tuple has no user override.
pub fn instantiate(row: &BakedRow, provider: &BakedProvider, display_order: u16) -> ProviderEntry {
    ProviderEntry {
        vendor: row.vendor.to_string(),
        model: row.model.to_string(),
        cli: provider.cli,
        launch_name: provider.launch_name.to_string(),
        enabled: true,
        free: provider.free,
        official: provider.official,
        quota_disabled: false,
        cheap_eligible: provider.cheap_eligible,
        tough_eligible: provider.tough_eligible,
        effort_eligible: provider.effort_eligible,
        effort_mapping: EffortMapping::new(
            provider.effort_cheap,
            provider.effort_normal,
            provider.effort_tough,
        ),
        display_order,
    }
}

/// Merge the baked-defaults table with the operator's user-supplied
/// providers list. The result is the unified list selection should
/// consume:
///
/// - For every baked provider tuple, the user's override wins entirely
///   when present (the user is responsible for re-stating fields they
///   want to keep — the loader is the layer that fills holes from
///   baked, not this resolver).
/// - User entries with no baked match are appended with
///   [`ADDITION_DISPLAY_ORDER`] as `display_order` if the user did not
///   set one explicitly.
///
/// The list is returned in baked order first (preserving each row's
/// authored sequence), with additions tacked on at the end.
pub fn merge_with_overrides(user: &[ProviderEntry]) -> Vec<ProviderEntry> {
    let mut result: Vec<ProviderEntry> = Vec::new();
    let mut consumed_user_indices: std::collections::HashSet<usize> =
        std::collections::HashSet::new();

    for row in BAKED_TABLE {
        for (idx, baked) in row.providers.iter().enumerate() {
            let display_order = idx as u16;
            let override_idx = user.iter().position(|u| {
                u.vendor == row.vendor
                    && u.model == row.model
                    && u.cli == baked.cli
                    && u.launch_name == baked.launch_name
            });
            match override_idx {
                Some(i) => {
                    consumed_user_indices.insert(i);
                    let mut entry = user[i].clone();
                    // Identity is shared; baked dictates display_order
                    // unless the user explicitly set a non-zero one.
                    if entry.display_order == 0 {
                        entry.display_order = display_order;
                    }
                    result.push(entry);
                }
                None => result.push(instantiate(row, baked, display_order)),
            }
        }
    }

    for (i, entry) in user.iter().enumerate() {
        if consumed_user_indices.contains(&i) {
            continue;
        }
        let mut addition = entry.clone();
        if addition.display_order == 0 {
            addition.display_order = ADDITION_DISPLAY_ORDER;
        }
        result.push(addition);
    }

    result
}

/// Look up a baked provider by `(vendor, model, cli, launch_name)`.
/// Returns `None` for additions — the spec rejects runtime heuristic
/// fallbacks, so callers MUST treat absence as "no baked defaults
/// available" rather than synthesizing one.
pub fn baked_for(
    vendor: &str,
    model: &str,
    cli: CliKind,
    launch_name: &str,
) -> Option<ProviderEntry> {
    for row in BAKED_TABLE {
        if row.vendor != vendor || row.model != model {
            continue;
        }
        for (idx, baked) in row.providers.iter().enumerate() {
            if baked.cli == cli && baked.launch_name == launch_name {
                return Some(instantiate(row, baked, idx as u16));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_table_seeds_known_claude_opus_with_max_tough_effort() {
        let entry = baked_for(
            "claude",
            "claude-opus-4-7",
            CliKind::Claude,
            "claude-opus-4-7",
        )
        .unwrap();
        assert!(
            entry.official,
            "claude/claude-opus-4-7 is intrinsically official"
        );
        assert!(entry.tough_eligible);
        assert!(entry.effort_eligible);
        assert_eq!(entry.effort_mapping.tough, "max");
    }

    #[test]
    fn baked_table_seeds_codex_with_xhigh_tough_effort() {
        let entry = baked_for("codex", "gpt-5", CliKind::Codex, "gpt-5").unwrap();
        assert_eq!(entry.effort_mapping.tough, "xhigh");
    }

    #[test]
    fn baked_for_returns_none_for_unknown_tuple() {
        assert!(baked_for("nope", "no-model", CliKind::Claude, "no-model").is_none());
    }

    #[test]
    fn merge_with_no_user_returns_full_baked_list() {
        let merged = merge_with_overrides(&[]);
        assert!(!merged.is_empty(), "baked table should not be empty");
        for entry in &merged {
            assert!(entry.enabled, "baked entries default to enabled");
        }
        // Every baked tuple appears at least once.
        for row in BAKED_TABLE {
            for baked in row.providers {
                let found = merged.iter().any(|e| {
                    e.vendor == row.vendor
                        && e.model == row.model
                        && e.cli == baked.cli
                        && e.launch_name == baked.launch_name
                });
                assert!(
                    found,
                    "merged list missing baked tuple ({}, {}, {:?}, {})",
                    row.vendor, row.model, baked.cli, baked.launch_name,
                );
            }
        }
    }

    #[test]
    fn merge_user_override_replaces_baked_props_for_matching_tuple() {
        let user = vec![ProviderEntry {
            vendor: "claude".to_string(),
            model: "claude-opus-4-7".to_string(),
            cli: CliKind::Claude,
            launch_name: "claude-opus-4-7".to_string(),
            enabled: false,
            free: false,
            official: true,
            quota_disabled: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_mapping: EffortMapping::new("low", "medium", "high"),
            display_order: 0,
        }];
        let merged = merge_with_overrides(&user);
        let opus = merged
            .iter()
            .find(|e| e.vendor == "claude" && e.model == "claude-opus-4-7")
            .expect("opus row must remain present after override");
        assert!(!opus.enabled, "user override flipped enabled to false");
        assert!(opus.quota_disabled, "user override forced quota_disabled");
        // Display order seeded from baked (idx 0) when the user left
        // it as the default zero.
        assert_eq!(opus.display_order, 0);
    }

    #[test]
    fn merge_user_addition_for_unknown_tuple_gets_addition_display_order() {
        let user = vec![ProviderEntry {
            vendor: "claude".to_string(),
            model: "claude-opus-4-7".to_string(),
            cli: CliKind::Opencode,
            launch_name: "claude-opus-4-7".to_string(),
            enabled: true,
            free: false,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: EffortMapping::default(),
            display_order: 0,
        }];
        let merged = merge_with_overrides(&user);
        let added = merged
            .iter()
            .find(|e| {
                e.vendor == "claude" && e.model == "claude-opus-4-7" && e.cli == CliKind::Opencode
            })
            .expect("addition must appear in merged list");
        assert_eq!(added.display_order, ADDITION_DISPLAY_ORDER);
    }

    #[test]
    fn merge_user_explicit_display_order_is_preserved() {
        let user = vec![ProviderEntry {
            vendor: "codex".to_string(),
            model: "gpt-5".to_string(),
            cli: CliKind::Opencode,
            launch_name: "gpt-5".to_string(),
            enabled: true,
            free: true,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: EffortMapping::default(),
            display_order: 7,
        }];
        let merged = merge_with_overrides(&user);
        let added = merged
            .iter()
            .find(|e| e.vendor == "codex" && e.model == "gpt-5" && e.cli == CliKind::Opencode)
            .unwrap();
        assert_eq!(added.display_order, 7);
    }
}
