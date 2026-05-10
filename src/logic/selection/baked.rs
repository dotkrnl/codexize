//! Baked default provider table for known dashboard `(model)` rows. The
//! table seeds every provider's per-tuple knobs (eligibility, effort
//! mapping, official/free flags) so the operator can override individual
//! fields from TOML without losing the rest of the baked defaults.
//!
//! Identity is `(cli, launch_name)`; `model` and `subscription` are
//! properties of the row/provider.
//!
//! Naming conventions: every `model` here matches the dashboard's
//! `entry.name` (the lowercased ipbr `display_name`). `launch_name` is the
//! literal CLI argument and may differ from `model` (for example
//! `kimi-latest` or qualified `opencode-go/...` routes). The live quota
//! provider returns values under `quota_lookup_key.unwrap_or(launch_name)`;
//! there is no normalization layer. Shared-pool subscriptions
//! (claude/kimi/opencode) point `quota_lookup_key` at their pool sentinel.
//!
//! Resolution rules:
//! - User entries with the same `(cli, launch_name)` as a baked entry
//!   **override** the baked properties.
//! - User entries whose identity is not in the baked table are
//!   **additions** (`display_order = u16::MAX`).
//! - Baked entries cannot be removed; setting `enabled = false` is the
//!   only way to take one out of selection.
//!
//! There is no runtime name-heuristic fallback: a model that isn't in
//! the baked table and has no user provider has zero candidates.

use crate::data::config::schema::{EffortMapping, ProviderEntry};
use crate::selection::{CliKind, SubscriptionKind};

/// One row in the baked defaults table — a model plus its ordered list
/// of baked providers. The ordering of `providers` drives the seeded
/// `display_order`.
pub struct BakedRow {
    pub model: &'static str,
    pub providers: &'static [BakedProvider],
}

/// One baked provider entry. Identity is `(cli, launch_name)`.
pub struct BakedProvider {
    pub cli: CliKind,
    pub launch_name: &'static str,
    pub subscription: SubscriptionKind,
    pub free: bool,
    pub official: bool,
    pub cheap_eligible: bool,
    pub tough_eligible: bool,
    pub effort_eligible: bool,
    pub effort_cheap: &'static str,
    pub effort_normal: &'static str,
    pub effort_tough: &'static str,
    pub quota_lookup_key: Option<&'static str>,
}

/// Sentinel display order for user-additions with no baked counterpart.
pub const ADDITION_DISPLAY_ORDER: u16 = u16::MAX;

/// Static baked-defaults table — 29 hand-curated provider entries
/// mirroring the ipbr scoreboard: 17 direct-subscription routes
/// (Claude opus/sonnet, Codex gpt-5.x, Gemini variants, Kimi k2.6 via
/// the Moonshot route) plus 12 opencode-go routes (deepseek, glm,
/// kimi-k2.5, kimi-k2.6, mimo, minimax, qwen) sharing the
/// `"opencode-shared"` quota pool. kimi-k2.6 stacks both the direct
/// Moonshot route and an opencode-go alternate. Models that the live
/// `opencode` CLI does not advertise (verified via `opencode models`)
/// are not baked here even if they appear on the IPBR scoreboard,
/// since launching them errors with `ProviderModelNotFoundError`.
pub const BAKED_TABLE: &[BakedRow] = &[
    // --- Claude opus (4 rows): tough_eligible=true, cheap_eligible=false, effort_tough="max" ---
    BakedRow {
        model: "claude-opus-4.1",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-opus-4.1",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    BakedRow {
        model: "claude-opus-4.5",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-opus-4.5",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    BakedRow {
        model: "claude-opus-4.6",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-opus-4.6",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    BakedRow {
        model: "claude-opus-4.7",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-opus-4.7",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    // --- Claude sonnet (3 rows): both cheap_eligible and tough_eligible, effort_tough="max" ---
    BakedRow {
        model: "claude-sonnet-4",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-sonnet-4",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    BakedRow {
        model: "claude-sonnet-4.5",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-sonnet-4.5",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    BakedRow {
        model: "claude-sonnet-4.6",
        providers: &[BakedProvider {
            cli: CliKind::Claude,
            launch_name: "claude-sonnet-4.6",
            subscription: SubscriptionKind::Claude,
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "max",
            quota_lookup_key: Some("claude-shared"),
        }],
    },
    // --- Codex gpt-5.x (4 rows): tough_eligible=true, cheap_eligible=false, effort_tough="xhigh" ---
    BakedRow {
        model: "gpt-5.2",
        providers: &[BakedProvider {
            cli: CliKind::Codex,
            launch_name: "gpt-5.2",
            subscription: SubscriptionKind::Codex,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "xhigh",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gpt-5.3-codex",
        providers: &[BakedProvider {
            cli: CliKind::Codex,
            launch_name: "gpt-5.3-codex",
            subscription: SubscriptionKind::Codex,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "xhigh",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gpt-5.4",
        providers: &[BakedProvider {
            cli: CliKind::Codex,
            launch_name: "gpt-5.4",
            subscription: SubscriptionKind::Codex,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "xhigh",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gpt-5.5",
        providers: &[BakedProvider {
            cli: CliKind::Codex,
            launch_name: "gpt-5.5",
            subscription: SubscriptionKind::Codex,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "xhigh",
            quota_lookup_key: None,
        }],
    },
    // --- Gemini (5 rows): effort_eligible=false, effort_tough="high" ---
    BakedRow {
        model: "gemini-2.5-flash",
        providers: &[BakedProvider {
            cli: CliKind::Gemini,
            launch_name: "gemini-2.5-flash",
            subscription: SubscriptionKind::Gemini,
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gemini-2.5-pro",
        providers: &[BakedProvider {
            cli: CliKind::Gemini,
            launch_name: "gemini-2.5-pro",
            subscription: SubscriptionKind::Gemini,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gemini-3-flash",
        providers: &[BakedProvider {
            cli: CliKind::Gemini,
            launch_name: "gemini-3-flash",
            subscription: SubscriptionKind::Gemini,
            free: false,
            official: true,
            cheap_eligible: true,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gemini-3-pro",
        providers: &[BakedProvider {
            cli: CliKind::Gemini,
            launch_name: "gemini-3-pro",
            subscription: SubscriptionKind::Gemini,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: None,
        }],
    },
    BakedRow {
        model: "gemini-3.1-pro-preview",
        providers: &[BakedProvider {
            cli: CliKind::Gemini,
            launch_name: "gemini-3.1-pro-preview",
            subscription: SubscriptionKind::Gemini,
            free: false,
            official: true,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: None,
        }],
    },
    // --- Kimi (2 rows): direct Moonshot route via Kimi CLI on kimi-latest,
    // plus an opencode-go route on each kimi model the live `opencode models`
    // command advertises.
    BakedRow {
        model: "kimi-k2.5",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/kimi-k2.5",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: true,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "kimi-k2.6",
        providers: &[
            BakedProvider {
                cli: CliKind::Kimi,
                launch_name: "kimi-latest",
                subscription: SubscriptionKind::Kimi,
                free: false,
                official: true,
                cheap_eligible: true,
                tough_eligible: false,
                effort_eligible: false,
                effort_cheap: "low",
                effort_normal: "medium",
                effort_tough: "high",
                quota_lookup_key: Some("kimi-shared"),
            },
            BakedProvider {
                cli: CliKind::Opencode,
                launch_name: "opencode-go/kimi-k2.6",
                subscription: SubscriptionKind::OpencodeGo,
                free: false,
                official: false,
                cheap_eligible: true,
                tough_eligible: false,
                effort_eligible: false,
                effort_cheap: "low",
                effort_normal: "medium",
                effort_tough: "high",
                quota_lookup_key: Some("opencode-shared"),
            },
        ],
    },
    // --- Opencode-go (11 rows): qualified launch_name, quota_lookup_key="opencode-shared" ---
    BakedRow {
        model: "deepseek-v4-flash",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/deepseek-v4-flash",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "deepseek-v4-pro",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/deepseek-v4-pro",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "minimax-m2.5",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/minimax-m2.5",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "minimax-m2.7",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/minimax-m2.7",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "qwen3.5-plus",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/qwen3.5-plus",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "qwen3.6-plus",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/qwen3.6-plus",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "mimo-v2.5",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/mimo-v2.5",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "mimo-v2.5-pro",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/mimo-v2.5-pro",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "glm-5",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/glm-5",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
    BakedRow {
        model: "glm-5.1",
        providers: &[BakedProvider {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/glm-5.1",
            subscription: SubscriptionKind::OpencodeGo,
            free: false,
            official: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_cheap: "low",
            effort_normal: "medium",
            effort_tough: "high",
            quota_lookup_key: Some("opencode-shared"),
        }],
    },
];

/// Materialize a baked provider as a concrete [`ProviderEntry`].
pub fn instantiate(row: &BakedRow, provider: &BakedProvider, display_order: u16) -> ProviderEntry {
    ProviderEntry {
        cli: provider.cli,
        launch_name: provider.launch_name.to_string(),
        model: row.model.to_string(),
        subscription: provider.subscription,
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
        quota_lookup_key: provider.quota_lookup_key.map(|s| s.to_string()),
        display_order,
    }
}

/// Merge the baked-defaults table with the operator's user-supplied
/// providers list. Identity is `(cli, launch_name)`.
pub fn merge_with_overrides(user: &[ProviderEntry]) -> Vec<ProviderEntry> {
    let mut result: Vec<ProviderEntry> = Vec::new();
    let mut consumed_user_indices: std::collections::HashSet<usize> =
        std::collections::HashSet::new();

    for row in BAKED_TABLE {
        for (idx, baked) in row.providers.iter().enumerate() {
            let display_order = idx as u16;
            let override_idx = user
                .iter()
                .position(|u| u.cli == baked.cli && u.launch_name == baked.launch_name);
            match override_idx {
                Some(i) => {
                    consumed_user_indices.insert(i);
                    let mut entry = user[i].clone();
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

/// Look up a baked provider by `(model, cli, launch_name)`.
pub fn baked_for(model: &str, cli: CliKind, launch_name: &str) -> Option<ProviderEntry> {
    for row in BAKED_TABLE {
        if row.model != model {
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

/// Return the baked row model for a provider identity, if that identity is
/// built in. User overrides must keep this canonical model unchanged.
pub fn model_for_identity(cli: CliKind, launch_name: &str) -> Option<&'static str> {
    for row in BAKED_TABLE {
        if row
            .providers
            .iter()
            .any(|provider| provider.cli == cli && provider.launch_name == launch_name)
        {
            return Some(row.model);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_table_seeds_known_claude_opus_with_max_tough_effort() {
        let entry = baked_for("claude-opus-4.7", CliKind::Claude, "claude-opus-4.7").unwrap();
        assert!(
            entry.official,
            "claude/claude-opus-4.7 is intrinsically official"
        );
        assert!(entry.tough_eligible);
        assert!(entry.effort_eligible);
        assert_eq!(entry.effort_mapping.tough, "max");
        assert_eq!(
            entry.quota_lookup_key.as_deref(),
            Some("claude-shared"),
            "claude rows route quota lookups to the shared pool sentinel"
        );
    }

    #[test]
    fn baked_table_seeds_codex_with_xhigh_tough_effort() {
        let entry = baked_for("gpt-5.4", CliKind::Codex, "gpt-5.4").unwrap();
        assert_eq!(entry.effort_mapping.tough, "xhigh");
    }

    #[test]
    fn baked_table_seeds_kimi_with_launch_name_kimi_latest() {
        let entry = baked_for("kimi-k2.6", CliKind::Kimi, "kimi-latest").unwrap();
        assert_eq!(entry.model, "kimi-k2.6");
        assert_eq!(entry.launch_name, "kimi-latest");
        assert!(entry.cheap_eligible);
        assert!(!entry.tough_eligible);
        assert!(!entry.effort_eligible);
        assert_eq!(entry.quota_lookup_key.as_deref(), Some("kimi-shared"));
    }

    #[test]
    fn baked_table_seeds_kimi_via_opencode_go_for_k25_and_k26() {
        // `opencode models` advertises both kimi-k2.5 and kimi-k2.6, so
        // both must be reachable via the opencode-go route. kimi-k2.6
        // additionally keeps its direct Moonshot route — see the
        // multi-provider entry above.
        let k25 = baked_for("kimi-k2.5", CliKind::Opencode, "opencode-go/kimi-k2.5")
            .expect("kimi-k2.5 via opencode-go");
        assert_eq!(k25.subscription, SubscriptionKind::OpencodeGo);
        assert_eq!(k25.quota_lookup_key.as_deref(), Some("opencode-shared"));

        let k26 = baked_for("kimi-k2.6", CliKind::Opencode, "opencode-go/kimi-k2.6")
            .expect("kimi-k2.6 via opencode-go");
        assert_eq!(k26.subscription, SubscriptionKind::OpencodeGo);
        assert!(!k26.official, "opencode-go is the unofficial alt route");
    }

    #[test]
    fn baked_table_seeds_opencode_go_with_shared_quota_key() {
        let entry = baked_for(
            "deepseek-v4-flash",
            CliKind::Opencode,
            "opencode-go/deepseek-v4-flash",
        )
        .unwrap();
        assert!(!entry.official);
        assert_eq!(entry.quota_lookup_key.as_deref(), Some("opencode-shared"));
        assert_eq!(entry.subscription, SubscriptionKind::OpencodeGo);
    }

    #[test]
    fn baked_table_has_twenty_nine_provider_entries() {
        // 16 direct-route entries (Claude/GPT/Gemini) + 1 direct kimi-k2.6
        // via Moonshot + 11 opencode-go entries + 1 alt opencode-go route
        // stacked on kimi-k2.6 = 29 total. Verified against
        // `opencode models` 2026-05.
        assert_eq!(
            BAKED_TABLE.iter().map(|r| r.providers.len()).sum::<usize>(),
            29
        );
    }

    #[test]
    fn baked_for_returns_none_for_unknown_tuple() {
        assert!(baked_for("no-model", CliKind::Claude, "no-model").is_none());
    }

    #[test]
    fn merge_with_no_user_returns_full_baked_list() {
        let merged = merge_with_overrides(&[]);
        assert!(!merged.is_empty(), "baked table should not be empty");
        for entry in &merged {
            assert!(entry.enabled, "baked entries default to enabled");
        }
        for row in BAKED_TABLE {
            for baked in row.providers {
                let found = merged.iter().any(|e| {
                    e.model == row.model && e.cli == baked.cli && e.launch_name == baked.launch_name
                });
                assert!(
                    found,
                    "merged list missing baked tuple ({}, {:?}, {})",
                    row.model, baked.cli, baked.launch_name,
                );
            }
        }
    }

    #[test]
    fn merge_user_override_replaces_baked_props_for_matching_tuple() {
        let user = vec![ProviderEntry {
            cli: CliKind::Claude,
            launch_name: "claude-opus-4.7".to_string(),
            model: "claude-opus-4.7".to_string(),
            subscription: SubscriptionKind::Claude,
            enabled: false,
            free: false,
            official: true,
            quota_disabled: true,
            cheap_eligible: false,
            tough_eligible: true,
            effort_eligible: true,
            effort_mapping: EffortMapping::new("low", "medium", "high"),
            quota_lookup_key: None,
            display_order: 0,
        }];
        let merged = merge_with_overrides(&user);
        let opus = merged
            .iter()
            .find(|e| e.cli == CliKind::Claude && e.launch_name == "claude-opus-4.7")
            .expect("opus row must remain present after override");
        assert!(!opus.enabled, "user override flipped enabled to false");
        assert!(opus.quota_disabled, "user override forced quota_disabled");
        assert_eq!(opus.display_order, 0);
    }

    #[test]
    fn merge_user_addition_for_unknown_identity_gets_addition_display_order() {
        let user = vec![ProviderEntry {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/claude-opus-4.7".to_string(),
            model: "claude-opus-4.7".to_string(),
            subscription: SubscriptionKind::OpencodeGo,
            enabled: true,
            free: false,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: EffortMapping::default(),
            quota_lookup_key: None,
            display_order: 0,
        }];
        let merged = merge_with_overrides(&user);
        let added = merged
            .iter()
            .find(|e| e.cli == CliKind::Opencode && e.launch_name == "opencode-go/claude-opus-4.7")
            .expect("addition must appear in merged list");
        assert_eq!(added.display_order, ADDITION_DISPLAY_ORDER);
    }

    #[test]
    fn merge_user_explicit_display_order_is_preserved() {
        let user = vec![ProviderEntry {
            cli: CliKind::Opencode,
            launch_name: "opencode-go/gpt-5".to_string(),
            model: "gpt-5".to_string(),
            subscription: SubscriptionKind::OpencodeGo,
            enabled: true,
            free: true,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: EffortMapping::default(),
            quota_lookup_key: None,
            display_order: 7,
        }];
        let merged = merge_with_overrides(&user);
        let added = merged
            .iter()
            .find(|e| e.cli == CliKind::Opencode && e.launch_name == "opencode-go/gpt-5")
            .unwrap();
        assert_eq!(added.display_order, 7);
    }

    #[test]
    fn baked_table_identity_is_unique() {
        use std::collections::HashMap;
        let mut seen: HashMap<(CliKind, &str), (&str, SubscriptionKind)> = HashMap::new();
        for row in BAKED_TABLE {
            for baked in row.providers {
                let key = (baked.cli, baked.launch_name);
                if let Some(prev) = seen.get(&key) {
                    panic!(
                        "duplicate baked identity (cli={:?}, launch_name={:?}): row={:?} (prev row={:?})",
                        baked.cli, baked.launch_name, row.model, prev.0
                    );
                }
                seen.insert(key, (row.model, baked.subscription));
            }
        }
    }
}
