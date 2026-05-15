//! Providers sub-panel widget.
//!
//! Unified per-tuple provider list. Identity is `(cli, launch_name)`; the
//! row label shows `(subscription, model)`. Editing happens through the
//! per-provider detail drawer (see `render_provider_detail_overlay`),
//! not through inline column-cycling.

use crate::data::config::Config;
use crate::data::config::schema::{EffortMapping, Override, ProviderEntry};
use crate::logic::selection::baked;
use crate::selection::{CliKind, SubscriptionKind};
use super::pad_right;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const COLOR_FOCUS: Color = Color::Cyan;
const COLOR_OVERRIDE: Color = Color::Yellow;
const COLOR_DIM: Color = Color::Gray;
const COLOR_OK: Color = Color::Green;
/// Subscription label color — visually distinct from the primary
/// (cli/launch_name) text so the billing pool reads as a chip rather
/// than blurring into the entry's identity.
const COLOR_SUBSCRIPTION: Color = Color::Magenta;
/// Per-flag chip colors. Each enabled flag carries its own hue so
/// scanning a row's eligibility set reads at a glance. Disabled flags
/// don't render at all (avoids visual clutter). built-in and official
/// are implicit defaults and never get a chip.
const COLOR_FREE: Color = Color::Cyan;
const COLOR_CHEAP: Color = Color::Blue;
const COLOR_TOUGH: Color = Color::Red;
const COLOR_EFFORT: Color = Color::Yellow;
/// Padding column widths for entry rows. Sized to the longest current
/// label so nothing wraps the layout.
const SUB_COL_WIDTH: usize = 11; // "opencode-go"
const CLI_COL_WIDTH: usize = 8; // "opencode"

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
    /// Index into [`SUBSCRIPTION_OPTIONS`].
    pub(crate) subscription_idx: usize,
    /// Index into [`CLI_OPTIONS`].
    pub(crate) cli_idx: usize,
    /// When set, a dropdown popup is open for the named field. The cursor
    /// inside the dropdown lives in `dropdown_cursor`.
    pub(crate) open_dropdown: Option<AddProviderField>,
    pub(crate) dropdown_cursor: usize,
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
            official: false,
            free: false,
            available_models,
            selected_model_idx: 0,
            focus: AddProviderField::Model,
            subscription_idx,
            cli_idx,
            open_dropdown: None,
            dropdown_cursor: 0,
        }
    }

    /// Snapshot the current options for a dropdown-capable field, for the
    /// modal renderer to draw and the key handler to navigate.
    pub(crate) fn dropdown_options(&self, target: AddProviderField) -> Vec<String> {
        match target {
            AddProviderField::Model => self
                .available_models
                .iter()
                .map(|(_, m)| m.clone())
                .collect(),
            AddProviderField::Subscription => SUBSCRIPTION_OPTIONS
                .iter()
                .map(|s| {
                    crate::logic::selection::subscription::subscription_kind_to_str(*s).to_string()
                })
                .collect(),
            AddProviderField::Cli => CLI_OPTIONS.iter().map(|c| c.as_str().to_string()).collect(),
            AddProviderField::Official | AddProviderField::Free | AddProviderField::LaunchName => {
                Vec::new()
            }
        }
    }

    /// Open the dropdown for `target` and position the cursor on the
    /// currently selected value.
    pub(crate) fn open_dropdown(&mut self, target: AddProviderField) {
        self.dropdown_cursor = match target {
            AddProviderField::Model => self.selected_model_idx,
            AddProviderField::Subscription => self.subscription_idx,
            AddProviderField::Cli => self.cli_idx,
            AddProviderField::Official | AddProviderField::Free | AddProviderField::LaunchName => 0,
        };
        self.open_dropdown = Some(target);
    }

    /// Apply the value under `dropdown_cursor` to the editor's selected
    /// state. Call this when the user presses Enter inside the dropdown.
    pub(crate) fn commit_dropdown(&mut self) {
        let Some(target) = self.open_dropdown else {
            return;
        };
        let cursor = self.dropdown_cursor;
        match target {
            AddProviderField::Model => {
                if let Some((s, m)) = self.available_models.get(cursor).cloned() {
                    self.selected_model_idx = cursor;
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
            }
            AddProviderField::Subscription => {
                if cursor < SUBSCRIPTION_OPTIONS.len() {
                    self.subscription_idx = cursor;
                    self.subscription =
                        crate::logic::selection::subscription::subscription_kind_to_str(
                            SUBSCRIPTION_OPTIONS[self.subscription_idx],
                        )
                        .to_string();
                }
            }
            AddProviderField::Cli => {
                if cursor < CLI_OPTIONS.len() {
                    self.cli_idx = cursor;
                    self.cli = CLI_OPTIONS[self.cli_idx];
                }
            }
            AddProviderField::Official | AddProviderField::Free | AddProviderField::LaunchName => {}
        }
        self.open_dropdown = None;
    }

    pub(crate) fn close_dropdown(&mut self) {
        self.open_dropdown = None;
    }

    pub(crate) fn commit(&self, config: &mut Config) -> bool {
        let trimmed_launch = self.launch_name.trim();
        let trimmed_model = self.model.trim();
        if trimmed_launch.is_empty() || self.subscription.is_empty() || trimmed_model.is_empty() {
            return false;
        }
        let Some(subscription) = SUBSCRIPTION_OPTIONS.get(self.subscription_idx).copied() else {
            return false;
        };

        let new_entry = ProviderEntry {
            cli: self.cli,
            launch_name: trimmed_launch.to_string(),
            model: trimmed_model.to_string(),
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
    /// Top-level vendor section. `folded` is true when the user has
    /// collapsed the section — only this header line renders, all the
    /// child models and providers are filtered out by `get_lines`.
    VendorHeader {
        vendor: String,
        folded: bool,
    },
    /// Model header nested under a vendor — purely structural, never
    /// folded or actionable. Cursor navigation skips it.
    ModelHeader {
        model: String,
    },
    Provider {
        entry: ProviderEntry,
        is_baked: bool,
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

pub(crate) fn get_lines(
    config: &Config,
    folded_vendors: &std::collections::BTreeSet<String>,
) -> Vec<ProvidersLine> {
    // Group rows by (vendor, model) so a single model collects every way
    // to launch it. The list is emitted as a 3-level tree (VendorHeader
    // · ModelHeader · Provider rows); folded vendors emit only their
    // header. Model headers don't fold — they're structural breadcrumbs.
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

/// Set of every vendor name reachable from the current config — used as
/// the panel's "fold everything by default" seed.
pub(crate) fn all_vendors(config: &Config) -> std::collections::BTreeSet<String> {
    let providers = baked::merge_with_overrides(config.providers.value());
    providers
        .iter()
        .map(|e| vendor_for(&e.model, e.subscription))
        .collect()
}

/// Compact one-line render of a provider list entry. Group headers and the
/// trailing "Add provider" action use distinct visual treatments so the
/// flat row stream still scans as a hierarchy.
pub(crate) fn format_line(line: &ProvidersLine, focused: bool, _width: usize) -> Line<'static> {
    match line {
        ProvidersLine::VendorHeader { vendor, folded } => {
            // Chevron flips with fold state so users see the affordance
            // at a glance: ▾ open, ▸ closed (both spaced before vendor).
            let chevron = if *folded { "▸" } else { "▾" };
            let focus_glyph = if focused {
                Span::styled(
                    "▌".to_string(),
                    Style::default()
                        .fg(COLOR_FOCUS)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(" ")
            };
            let vendor_style = if focused {
                Style::default()
                    .fg(COLOR_SUBSCRIPTION)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default()
                    .fg(COLOR_SUBSCRIPTION)
                    .add_modifier(Modifier::BOLD)
            };
            Line::from(vec![
                focus_glyph,
                Span::styled(
                    format!("{chevron} "),
                    Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
                ),
                Span::styled(vendor.clone(), vendor_style),
            ])
        }
        ProvidersLine::ModelHeader { model } => Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "▾ ".to_string(),
                Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                model.clone(),
                Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
            ),
        ]),
        ProvidersLine::Provider { entry, is_baked } => {
            let focus_glyph = if focused {
                Span::styled(
                    "▌".to_string(),
                    Style::default()
                        .fg(COLOR_FOCUS)
                        .add_modifier(Modifier::BOLD),
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
                Style::default()
                    .fg(COLOR_FOCUS)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            // built-in is the implicit default — no chip. Custom entries
            // (user-added overrides) keep their yellow chip so they
            // stand out in the list.
            let custom_chip = if *is_baked { None } else { Some("custom") };
            let free = entry.free;

            let subscription_label =
                crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription);

            // Layout: indent (4) · focus (1) · " " · enabled (1) · " " ·
            // subscription (padded SUB_COL_WIDTH, magenta) · "  " ·
            // cli (padded CLI_COL_WIDTH, primary) · "  " · launch_name ·
            // "  " · chips. Aligned so subscription and cli columns stay
            // straight across the list.
            let mut spans: Vec<Span<'static>> = vec![
                Span::raw("    "),
                focus_glyph,
                Span::raw(" "),
                enabled_glyph,
                Span::raw(" "),
                Span::styled(
                    pad_right(subscription_label, SUB_COL_WIDTH),
                    Style::default().fg(COLOR_SUBSCRIPTION),
                ),
                Span::raw("  "),
                Span::styled(pad_right(entry.cli.as_str(), CLI_COL_WIDTH), primary_style),
                Span::raw("  "),
                Span::styled(entry.launch_name.clone(), primary_style),
            ];

            // Conditional, color-coded chip strip. Each flag renders only
            // when on; built-in/official are implicit and never shown.
            // Colors: free=cyan (no cost), cheap=blue (budget tier),
            // tough=red (heavy hitter), effort=yellow (effort-adjustable).
            let mut chips: Vec<(String, Color)> = Vec::new();
            if let Some(label) = custom_chip {
                chips.push((label.to_string(), COLOR_OVERRIDE));
            }
            if free {
                chips.push(("free".to_string(), COLOR_FREE));
            }
            if entry.cheap_eligible {
                chips.push(("cheap".to_string(), COLOR_CHEAP));
            }
            if entry.tough_eligible {
                chips.push(("tough".to_string(), COLOR_TOUGH));
            }
            if entry.effort_eligible {
                chips.push(("effort".to_string(), COLOR_EFFORT));
            }
            for (idx, (label, color)) in chips.into_iter().enumerate() {
                let separator = if idx == 0 { "  " } else { " · " };
                spans.push(Span::styled(
                    separator.to_string(),
                    Style::default().fg(COLOR_DIM),
                ));
                spans.push(Span::styled(label, Style::default().fg(color)));
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
                    Style::default()
                        .fg(COLOR_FOCUS)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(" ")
            };
            let style = if focused {
                Style::default()
                    .fg(COLOR_FOCUS)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLOR_DIM)
            };
            Line::from(vec![
                Span::raw("  "),
                focus,
                Span::raw(" "),
                Span::styled("+ New model".to_string(), style),
            ])
        }
    }
}

