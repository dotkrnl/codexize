//! Providers sub-panel widget.
//!
//! Unified per-tuple provider list. Identity is `(vendor, model, cli, launch_name)`.
//! The panel allows toggling properties for baked and user-added providers.

use crate::data::config::Config;
use crate::data::config::schema::{EffortMapping, Override, ProviderEntry};
use crate::logic::selection::assemble::parse_subscription_str;
use crate::logic::selection::baked;
use crate::selection::{CliKind, SubscriptionKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvidersEditor {
    pub(crate) vendor: String,
    pub(crate) model: String,
    pub(crate) cli: CliKind,
    pub(crate) launch_name: String,
    pub(crate) available_models: Vec<(String, String)>, // (vendor, model)
    pub(crate) selected_model_idx: usize,
}

impl ProvidersEditor {
    pub(crate) fn new(available_models: Vec<(String, String)>) -> Self {
        let (vendor, model) = available_models.first().cloned().unwrap_or_default();
        Self {
            vendor,
            model,
            cli: CliKind::Opencode,
            launch_name: String::new(),
            available_models,
            selected_model_idx: 0,
        }
    }

    pub(crate) fn commit(&self, config: &mut Config) -> bool {
        let trimmed_launch = self.launch_name.trim();
        if trimmed_launch.is_empty() || self.vendor.is_empty() || self.model.is_empty() {
            return false;
        }
        let subscription = parse_subscription_str(&self.vendor).unwrap_or(SubscriptionKind::Direct);

        let new_entry = ProviderEntry {
            cli: self.cli,
            launch_name: trimmed_launch.to_string(),
            model: self.model.clone(),
            subscription,
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
        };

        let mut existing = config.providers.value().clone();
        // Check for duplicates
        if existing
            .iter()
            .any(|e| e.identity() == new_entry.identity())
        {
            return false;
        }
        existing.push(new_entry);
        config.providers = Override::explicit(existing);
        true
    }
}

pub(crate) enum ProvidersLine {
    GroupHeader {
        vendor: String,
        model: String,
    },
    Provider {
        entry: ProviderEntry,
        is_baked: bool,
        baked_free: bool,
        baked_official: bool,
    },
    AddAction,
}

pub(crate) fn get_lines(config: &Config) -> Vec<ProvidersLine> {
    let providers = baked::merge_with_overrides(config.providers.value());
    let mut lines = Vec::new();

    let mut current_group: Option<(String, String)> = None;

    for entry in providers {
        let vendor_label =
            crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription)
                .to_string();
        let group = (vendor_label, entry.model.clone());
        if current_group.as_ref() != Some(&group) {
            lines.push(ProvidersLine::GroupHeader {
                vendor: group.0.clone(),
                model: group.1.clone(),
            });
            current_group = Some(group.clone());
        }

        let baked = baked::baked_for(&group.1, entry.cli, &entry.launch_name);
        lines.push(ProvidersLine::Provider {
            is_baked: baked.is_some(),
            baked_free: baked.as_ref().is_some_and(|b| b.free),
            baked_official: baked.as_ref().is_some_and(|b| b.official),
            entry,
        });
    }

    lines.push(ProvidersLine::AddAction);
    lines
}

pub(crate) fn format_line(
    line: &ProvidersLine,
    selected: bool,
    prop_selected: usize,
    width: usize,
) -> String {
    match line {
        ProvidersLine::GroupHeader { vendor, model } => {
            let text = format!("{} / {}", vendor, model);
            format!("  {}", text)
        }
        ProvidersLine::Provider {
            entry,
            is_baked,
            baked_free,
            baked_official,
        } => {
            let marker = if selected { ">" } else { " " };

            let enabled_str = if entry.enabled { "[x]" } else { "[ ]" };
            let enabled = if selected && prop_selected == 0 {
                format!("*{}*", enabled_str)
            } else {
                enabled_str.to_string()
            };

            let official_str = if *is_baked {
                if *baked_official {
                    "(x) official"
                } else {
                    "( ) official"
                }
            } else {
                if entry.official {
                    "[x] official"
                } else {
                    "[ ] official"
                }
            };
            let official = if selected && prop_selected == 1 {
                format!("*{}*", official_str)
            } else {
                official_str.to_string()
            };

            let free_str = if *is_baked {
                if *baked_free { "(x) free" } else { "( ) free" }
            } else {
                if entry.free { "[x] free" } else { "[ ] free" }
            };
            let free = if selected && prop_selected == 2 {
                format!("*{}*", free_str)
            } else {
                free_str.to_string()
            };

            let quota_str = if entry.quota_disabled {
                "[x] no-quota"
            } else {
                "[ ] no-quota"
            };
            let quota = if selected && prop_selected == 3 {
                format!("*{}*", quota_str)
            } else {
                quota_str.to_string()
            };

            let mut text = format!(
                "{} {} {} · {} · {} · {} · {}",
                marker,
                enabled,
                entry.cli.as_str(),
                entry.launch_name,
                official,
                free,
                quota
            );

            // Add eligibility flags
            let cheap_str = if entry.cheap_eligible { "c" } else { "-" };
            let cheap = if selected && prop_selected == 4 {
                format!("*{}*", cheap_str)
            } else {
                cheap_str.to_string()
            };

            let tough_str = if entry.tough_eligible { "t" } else { "-" };
            let tough = if selected && prop_selected == 5 {
                format!("*{}*", tough_str)
            } else {
                tough_str.to_string()
            };

            let effort_str = if entry.effort_eligible { "e" } else { "-" };
            let effort = if selected && prop_selected == 6 {
                format!("*{}*", effort_str)
            } else {
                effort_str.to_string()
            };

            text.push_str(&format!(" · {}{}{}", cheap, tough, effort));

            if text.len() > width {
                text.truncate(width);
            }
            format!("  {}", text)
        }
        ProvidersLine::AddAction => {
            let marker = if selected { ">" } else { " " };
            format!("  {} [ Add Provider ]", marker)
        }
    }
}
