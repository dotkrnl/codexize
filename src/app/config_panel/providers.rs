use crate::data::config::Config;
use crate::data::config::schema::{EffortMapping, Override, ProviderEntry};
use crate::logic::selection::baked;
use crate::selection::{CliKind, SubscriptionKind};

/// Which field of the Add Provider modal currently has the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddProviderField {
    Model,
    Subscription,
    Cli,
    Official,
    Free,
    LaunchName,
}

impl AddProviderField {
    pub(crate) fn next(self) -> Self {
        match self {
            AddProviderField::Model => AddProviderField::Subscription,
            AddProviderField::Subscription => AddProviderField::Cli,
            AddProviderField::Cli => AddProviderField::Official,
            AddProviderField::Official => AddProviderField::Free,
            AddProviderField::Free => AddProviderField::LaunchName,
            AddProviderField::LaunchName => AddProviderField::Model,
        }
    }
    pub(crate) fn prev(self) -> Self {
        match self {
            AddProviderField::Model => AddProviderField::LaunchName,
            AddProviderField::Subscription => AddProviderField::Model,
            AddProviderField::Cli => AddProviderField::Subscription,
            AddProviderField::Official => AddProviderField::Cli,
            AddProviderField::Free => AddProviderField::Official,
            AddProviderField::LaunchName => AddProviderField::Free,
        }
    }
}

const SUBSCRIPTION_OPTIONS: &[SubscriptionKind] = &[
    SubscriptionKind::Claude,
    SubscriptionKind::Codex,
    SubscriptionKind::Gemini,
    SubscriptionKind::Kimi,
    SubscriptionKind::OpencodeGo,
    SubscriptionKind::Direct,
];

const CLI_OPTIONS: &[CliKind] = &[
    CliKind::Claude,
    CliKind::Codex,
    CliKind::Gemini,
    CliKind::Kimi,
    CliKind::Opencode,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvidersEditor {
    pub(crate) subscription: String,
    pub(crate) model: String,
    pub(crate) cli: CliKind,
    pub(crate) launch_name: String,
    pub(crate) official: bool,
    pub(crate) free: bool,
    /// Unique (subscription, model) pairs derived from the baked + override
    /// universe. Used as the Model dropdown's option list.
    pub(crate) available_models: Vec<(String, String)>,
    pub(crate) selected_model_idx: usize,
    pub(crate) focus: AddProviderField,
    pub(crate) dropdown_cursor: usize,
    pub(crate) open_dropdown: Option<AddProviderField>,
}

impl ProvidersEditor {
    pub(crate) fn new(available_models: Vec<(String, String)>) -> Self {
        let (subscription, model) = available_models
            .first()
            .cloned()
            .unwrap_or_else(|| ("".to_string(), "".to_string()));
        Self {
            subscription,
            model,
            cli: CliKind::Claude,
            launch_name: String::new(),
            official: true,
            free: false,
            available_models,
            selected_model_idx: 0,
            focus: AddProviderField::Model,
            dropdown_cursor: 0,
            open_dropdown: None,
        }
    }

    pub(crate) fn open_dropdown(&mut self, field: AddProviderField) {
        self.open_dropdown = Some(field);
        self.dropdown_cursor = match field {
            AddProviderField::Model => self.selected_model_idx,
            AddProviderField::Subscription => SUBSCRIPTION_OPTIONS
                .iter()
                .position(|v| v.as_str() == self.subscription)
                .unwrap_or(0),
            AddProviderField::Cli => CLI_OPTIONS.iter().position(|v| v == &self.cli).unwrap_or(0),
            _ => 0,
        };
    }

    pub(crate) fn close_dropdown(&mut self) {
        self.open_dropdown = None;
    }

    pub(crate) fn dropdown_options(&self, field: AddProviderField) -> Vec<String> {
        match field {
            AddProviderField::Model => self
                .available_models
                .iter()
                .map(|(_, m)| m.clone())
                .collect(),
            AddProviderField::Subscription => SUBSCRIPTION_OPTIONS
                .iter()
                .map(|v| v.as_str().to_string())
                .collect(),
            AddProviderField::Cli => CLI_OPTIONS.iter().map(|v| v.as_str().to_string()).collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn commit_dropdown(&mut self) {
        let Some(field) = self.open_dropdown.take() else {
            return;
        };
        match field {
            AddProviderField::Model => {
                if let Some((v, m)) = self.available_models.get(self.dropdown_cursor) {
                    self.subscription = v.clone();
                    self.model = m.clone();
                    self.selected_model_idx = self.dropdown_cursor;
                }
            }
            AddProviderField::Subscription => {
                if let Some(v) = SUBSCRIPTION_OPTIONS.get(self.dropdown_cursor) {
                    self.subscription = v.as_str().to_string();
                }
            }
            AddProviderField::Cli => {
                if let Some(v) = CLI_OPTIONS.get(self.dropdown_cursor) {
                    self.cli = *v;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn commit(&self, config: &mut Config) -> bool {
        if self.launch_name.trim().is_empty() {
            return false;
        }
        let subscription =
            SubscriptionKind::parse(&self.subscription).unwrap_or(SubscriptionKind::Direct);
        let entry = ProviderEntry {
            cli: self.cli,
            launch_name: self.launch_name.clone(),
            model: self.model.clone(),
            subscription,
            enabled: true,
            free: self.free,
            official: self.official,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: EffortMapping::default(),
            quota_lookup_key: None,
            display_order: 0,
        };

        let mut existing = config.providers.value().clone();
        if existing.iter().any(|e| e.identity() == entry.identity()) {
            return false;
        }
        existing.push(entry);
        config.providers = Override::explicit(existing);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProvidersLine {
    VendorHeader {
        vendor: String,
        folded: bool,
    },
    ModelHeader {
        model: String,
    },
    Provider {
        is_baked: bool,
        entry: ProviderEntry,
    },
    AddAction,
}

pub(crate) fn get_lines(
    config: &Config,
    folded_vendors: &std::collections::BTreeSet<String>,
) -> Vec<ProvidersLine> {
    let providers = baked::merge_with_overrides(config.providers.value());
    let mut buckets: Vec<((String, String), Vec<ProviderEntry>)> = Vec::new();
    for entry in providers {
        let key = (
            vendor_for(&entry.model, entry.subscription),
            entry.model.clone(),
        );
        if let Some(bucket) = buckets.iter_mut().find(|(k, _)| *k == key) {
            bucket.1.push(entry);
        } else {
            buckets.push((key, vec![entry]));
        }
    }

    let mut lines = Vec::new();
    let mut current_vendor: Option<String> = None;
    let mut current_vendor_folded = false;
    for ((vendor, model), entries) in buckets {
        if current_vendor.as_deref() != Some(vendor.as_str()) {
            let folded = folded_vendors.contains(&vendor);
            lines.push(ProvidersLine::VendorHeader {
                vendor: vendor.clone(),
                folded,
            });
            current_vendor = Some(vendor.clone());
            current_vendor_folded = folded;
        }
        if current_vendor_folded {
            continue;
        }
        lines.push(ProvidersLine::ModelHeader {
            model: model.clone(),
        });
        for entry in entries {
            let baked = baked::baked_for(&model, entry.cli, &entry.launch_name);
            lines.push(ProvidersLine::Provider {
                is_baked: baked.is_some(),
                entry,
            });
        }
    }

    lines.push(ProvidersLine::AddAction);
    lines
}

pub(crate) fn all_vendors(config: &Config) -> std::collections::BTreeSet<String> {
    let providers = baked::merge_with_overrides(config.providers.value());
    providers
        .iter()
        .map(|e| vendor_for(&e.model, e.subscription))
        .collect()
}

fn vendor_for(model: &str, subscription: SubscriptionKind) -> String {
    if let Some(vendor) = crate::model_names::display_vendor(model) {
        return vendor.to_string();
    }
    crate::logic::selection::subscription::subscription_kind_to_str(subscription).to_string()
}
