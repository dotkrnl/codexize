//! Providers sub-panel widget.
//!
//! Unified per-tuple provider list. Identity is `(cli, launch_name)`; the
//! row label shows `(subscription, model)`. Editing happens through the
//! per-provider detail drawer (see `render_provider_detail_overlay`),
//! not through inline column-cycling.

use crate::data::config::Config;
use crate::data::config::schema::{EffortMapping, Override, ProviderEntry};
use crate::logic::selection::assemble::parse_subscription_str;
use crate::logic::selection::baked;
use crate::selection::{CliKind, SubscriptionKind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const COLOR_FOCUS: Color = Color::Cyan;
const COLOR_OVERRIDE: Color = Color::Yellow;
const COLOR_DIM: Color = Color::DarkGray;
const COLOR_OK: Color = Color::Green;

/// Which field of the Add Provider modal currently has the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddProviderField {
    Model,
    Subscription,
    Cli,
    LaunchName,
}

impl AddProviderField {
    pub(crate) fn next(self) -> Self {
        match self {
            AddProviderField::Model => AddProviderField::Subscription,
            AddProviderField::Subscription => AddProviderField::Cli,
            AddProviderField::Cli => AddProviderField::LaunchName,
            AddProviderField::LaunchName => AddProviderField::Model,
        }
    }
    pub(crate) fn prev(self) -> Self {
        match self {
            AddProviderField::Model => AddProviderField::LaunchName,
            AddProviderField::Subscription => AddProviderField::Model,
            AddProviderField::Cli => AddProviderField::Subscription,
            AddProviderField::LaunchName => AddProviderField::Cli,
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
    /// Unique (subscription, model) pairs derived from the baked + override
    /// universe. Used as suggestions for the Model picker — the user can
    /// also free-cycle subscription/cli to compose entries the universe
    /// doesn't already advertise (e.g. opencode-go for kimi-k2.6).
    pub(crate) available_models: Vec<(String, String)>,
    pub(crate) selected_model_idx: usize,
    pub(crate) focus: AddProviderField,
    /// Index into [`SUBSCRIPTION_OPTIONS`].
    pub(crate) subscription_idx: usize,
    /// Index into [`CLI_OPTIONS`].
    pub(crate) cli_idx: usize,
}

impl ProvidersEditor {
    pub(crate) fn new(available_models: Vec<(String, String)>) -> Self {
        let (subscription, model) = available_models.first().cloned().unwrap_or_default();
        let cli = CliKind::Opencode;
        let cli_idx = CLI_OPTIONS.iter().position(|c| *c == cli).unwrap_or(0);
        let subscription_idx = SUBSCRIPTION_OPTIONS
            .iter()
            .position(|s| {
                crate::logic::selection::subscription::subscription_kind_to_str(*s)
                    == subscription.as_str()
            })
            .unwrap_or(0);
        Self {
            subscription,
            model,
            cli,
            launch_name: String::new(),
            available_models,
            selected_model_idx: 0,
            focus: AddProviderField::Model,
            subscription_idx,
            cli_idx,
        }
    }

    pub(crate) fn cycle_focused(&mut self, delta: isize) {
        match self.focus {
            AddProviderField::Model => {
                if self.available_models.is_empty() {
                    return;
                }
                self.selected_model_idx =
                    super::wrap_index(self.selected_model_idx, self.available_models.len(), delta);
                let (s, m) = self.available_models[self.selected_model_idx].clone();
                self.subscription = s;
                self.model = m;
                self.subscription_idx = SUBSCRIPTION_OPTIONS
                    .iter()
                    .position(|sk| {
                        crate::logic::selection::subscription::subscription_kind_to_str(*sk)
                            == self.subscription.as_str()
                    })
                    .unwrap_or(self.subscription_idx);
            }
            AddProviderField::Subscription => {
                self.subscription_idx =
                    super::wrap_index(self.subscription_idx, SUBSCRIPTION_OPTIONS.len(), delta);
                self.subscription = crate::logic::selection::subscription::subscription_kind_to_str(
                    SUBSCRIPTION_OPTIONS[self.subscription_idx],
                )
                .to_string();
            }
            AddProviderField::Cli => {
                self.cli_idx = super::wrap_index(self.cli_idx, CLI_OPTIONS.len(), delta);
                self.cli = CLI_OPTIONS[self.cli_idx];
            }
            AddProviderField::LaunchName => {}
        }
    }

    pub(crate) fn commit(&self, config: &mut Config) -> bool {
        let trimmed_launch = self.launch_name.trim();
        let trimmed_model = self.model.trim();
        if trimmed_launch.is_empty() || self.subscription.is_empty() || trimmed_model.is_empty() {
            return false;
        }
        // Prefer the picker-tracked subscription (set by `cycle_focused`)
        // and fall back to parsing the label so legacy data round-trips.
        let subscription = SUBSCRIPTION_OPTIONS
            .get(self.subscription_idx)
            .copied()
            .or_else(|| parse_subscription_str(&self.subscription))
            .unwrap_or(SubscriptionKind::Direct);

        let new_entry = ProviderEntry {
            cli: self.cli,
            launch_name: trimmed_launch.to_string(),
            model: trimmed_model.to_string(),
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

/// Vendor key for a model. Curated models pull their vendor from the
/// model_names table (e.g. `claude`, `gpt`, `kimi`, `deepseek`); anything
/// else falls back to the subscription label so unknown additions still
/// land somewhere consistent.
pub(crate) fn vendor_for(model: &str, subscription: SubscriptionKind) -> String {
    if let Some(v) = crate::model_names::display_vendor(model) {
        return v.to_string();
    }
    crate::logic::selection::subscription::subscription_kind_to_str(subscription).to_string()
}

pub(crate) fn get_lines(config: &Config) -> Vec<ProvidersLine> {
    // Group rows by (vendor, model) so a single model collects every way
    // to launch it (one entry per subscription/CLI). The merge step yields
    // entries in a stable display_order, which we then re-sort by group
    // key while preserving relative order inside each group.
    let providers = baked::merge_with_overrides(config.providers.value());
    let mut buckets: Vec<((String, String), Vec<ProviderEntry>)> = Vec::new();
    for entry in providers {
        let key = (vendor_for(&entry.model, entry.subscription), entry.model.clone());
        if let Some(bucket) = buckets.iter_mut().find(|(k, _)| *k == key) {
            bucket.1.push(entry);
        } else {
            buckets.push((key, vec![entry]));
        }
    }

    let mut lines = Vec::new();
    for ((vendor, model), entries) in buckets {
        lines.push(ProvidersLine::GroupHeader {
            vendor: vendor.clone(),
            model: model.clone(),
        });
        for entry in entries {
            let baked = baked::baked_for(&model, entry.cli, &entry.launch_name);
            lines.push(ProvidersLine::Provider {
                is_baked: baked.is_some(),
                baked_free: baked.as_ref().is_some_and(|b| b.free),
                baked_official: baked.as_ref().is_some_and(|b| b.official),
                entry,
            });
        }
    }

    lines.push(ProvidersLine::AddAction);
    lines
}

/// Compact one-line render of a provider list entry. Group headers and the
/// trailing "Add provider" action use distinct visual treatments so the
/// flat row stream still scans as a hierarchy.
pub(crate) fn format_line(line: &ProvidersLine, focused: bool, _width: usize) -> Line<'static> {
    match line {
        ProvidersLine::GroupHeader { vendor, model } => {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "▾ ".to_string(),
                    Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} · {}", vendor, model),
                    Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
                ),
            ])
        }
        ProvidersLine::Provider {
            entry,
            is_baked,
            baked_free,
            baked_official,
        } => {
            let focus_glyph = if focused {
                Span::styled(
                    "▌".to_string(),
                    Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(" ")
            };

            let enabled_glyph = if entry.enabled {
                Span::styled("✓".to_string(), Style::default().fg(COLOR_OK))
            } else {
                Span::styled("✗".to_string(), Style::default().fg(COLOR_DIM))
            };

            let primary_style = if focused {
                Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let source_label = if *is_baked { "built-in" } else { "custom" };
            let source_style = if *is_baked {
                Style::default().fg(COLOR_DIM)
            } else {
                Style::default().fg(COLOR_OVERRIDE)
            };

            let official = if *is_baked { *baked_official } else { entry.official };
            let free = if *is_baked { *baked_free } else { entry.free };

            let subscription_label = crate::logic::selection::subscription::subscription_kind_to_str(
                entry.subscription,
            );

            let mut spans: Vec<Span<'static>> = Vec::new();
            // Indent extra so entries sit visually inside their group header.
            spans.push(Span::raw("    "));
            spans.push(focus_glyph);
            spans.push(Span::raw(" "));
            spans.push(enabled_glyph);
            spans.push(Span::raw(" "));
            // subscription · cli  launch_name — the user-facing way to think
            // about an entry: which billing pool, which CLI binary, which
            // model name passed at launch.
            spans.push(Span::styled(
                subscription_label.to_string(),
                Style::default().fg(COLOR_DIM),
            ));
            spans.push(Span::styled(
                "/".to_string(),
                Style::default().fg(COLOR_DIM),
            ));
            spans.push(Span::styled(entry.cli.as_str().to_string(), primary_style));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(entry.launch_name.clone(), primary_style));
            spans.push(Span::styled("  ".to_string(), Style::default()));
            spans.push(Span::styled(source_label.to_string(), source_style));
            spans.push(Span::styled(" · ".to_string(), Style::default().fg(COLOR_DIM)));
            spans.push(Span::styled(
                if official { "official" } else { "unofficial" }.to_string(),
                Style::default().fg(COLOR_DIM),
            ));
            spans.push(Span::styled(" · ".to_string(), Style::default().fg(COLOR_DIM)));
            spans.push(Span::styled(
                if free { "free" } else { "paid" }.to_string(),
                Style::default().fg(COLOR_DIM),
            ));
            for (label, on) in [
                ("cheap", entry.cheap_eligible),
                ("tough", entry.tough_eligible),
                ("effort", entry.effort_eligible),
            ] {
                spans.push(Span::styled(" · ".to_string(), Style::default().fg(COLOR_DIM)));
                let style = if on {
                    Style::default().fg(COLOR_OK)
                } else {
                    Style::default().fg(COLOR_DIM)
                };
                spans.push(Span::styled(label.to_string(), style));
            }
            if entry.quota_disabled {
                spans.push(Span::styled(
                    "  (no quota)".to_string(),
                    Style::default().fg(COLOR_OVERRIDE),
                ));
            }
            Line::from(spans)
        }
        ProvidersLine::AddAction => {
            let focus = if focused {
                Span::styled(
                    "▌".to_string(),
                    Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(" ")
            };
            let style = if focused {
                Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLOR_DIM)
            };
            Line::from(vec![
                Span::raw("  "),
                focus,
                Span::raw(" "),
                Span::styled("+ Add model provider".to_string(), style),
            ])
        }
    }
}
