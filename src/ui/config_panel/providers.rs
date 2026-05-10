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
/// Subscription label color — visually distinct from the primary
/// (cli/launch_name) text so the billing pool reads as a chip rather
/// than blurring into the entry's identity.
const COLOR_SUBSCRIPTION: Color = Color::Magenta;
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
            AddProviderField::Cli => {
                CLI_OPTIONS.iter().map(|c| c.as_str().to_string()).collect()
            }
            AddProviderField::LaunchName => Vec::new(),
        }
    }

    /// Open the dropdown for `target` and position the cursor on the
    /// currently selected value.
    pub(crate) fn open_dropdown(&mut self, target: AddProviderField) {
        self.dropdown_cursor = match target {
            AddProviderField::Model => self.selected_model_idx,
            AddProviderField::Subscription => self.subscription_idx,
            AddProviderField::Cli => self.cli_idx,
            AddProviderField::LaunchName => 0,
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
            AddProviderField::LaunchName => {}
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
    /// Top-level vendor section — rendered once per vendor, contains
    /// every model/provider grouped underneath.
    VendorHeader { vendor: String },
    /// Model header nested under a vendor — drops the redundant vendor
    /// name from the label since the vendor section above is already
    /// visible.
    ModelHeader { model: String },
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
    // to launch it. The provider list is then emitted as a 3-level tree:
    // VendorHeader · ModelHeader · Provider rows. The merge step yields
    // entries in stable display_order; we preserve relative order
    // inside each (vendor, model) bucket.
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
    let mut current_vendor: Option<String> = None;
    for ((vendor, model), entries) in buckets {
        if current_vendor.as_deref() != Some(vendor.as_str()) {
            lines.push(ProvidersLine::VendorHeader {
                vendor: vendor.clone(),
            });
            current_vendor = Some(vendor.clone());
        }
        lines.push(ProvidersLine::ModelHeader {
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
        ProvidersLine::VendorHeader { vendor } => Line::from(vec![
            Span::styled(
                "▾ ".to_string(),
                Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                vendor.clone(),
                Style::default()
                    .fg(COLOR_SUBSCRIPTION)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
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

            let subscription_label =
                crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription);

            // Layout: indent (4) · focus (1) · " " · enabled (1) · " " ·
            // subscription (padded SUB_COL_WIDTH, magenta) · "  " ·
            // cli (padded CLI_COL_WIDTH, primary) · "  " · launch_name ·
            // "  " · chips. Aligned so subscription and cli columns stay
            // straight across the list.
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::raw("    "));
            spans.push(focus_glyph);
            spans.push(Span::raw(" "));
            spans.push(enabled_glyph);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                pad_right(subscription_label, SUB_COL_WIDTH),
                Style::default().fg(COLOR_SUBSCRIPTION),
            ));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                pad_right(entry.cli.as_str(), CLI_COL_WIDTH),
                primary_style,
            ));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(entry.launch_name.clone(), primary_style));
            spans.push(Span::raw("  "));
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
                Span::styled("+ New model".to_string(), style),
            ])
        }
    }
}

fn pad_right(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let used = text.width();
    if used >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - used))
    }
}
