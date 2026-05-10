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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvidersEditor {
    pub(crate) subscription: String,
    pub(crate) model: String,
    pub(crate) cli: CliKind,
    pub(crate) launch_name: String,
    pub(crate) available_models: Vec<(String, String)>, // (subscription, model)
    pub(crate) selected_model_idx: usize,
}

impl ProvidersEditor {
    pub(crate) fn new(available_models: Vec<(String, String)>) -> Self {
        let (subscription, model) = available_models.first().cloned().unwrap_or_default();
        Self {
            subscription,
            model,
            cli: CliKind::Opencode,
            launch_name: String::new(),
            available_models,
            selected_model_idx: 0,
        }
    }

    pub(crate) fn commit(&self, config: &mut Config) -> bool {
        let trimmed_launch = self.launch_name.trim();
        if trimmed_launch.is_empty() || self.subscription.is_empty() || self.model.is_empty() {
            return false;
        }
        let subscription =
            parse_subscription_str(&self.subscription).unwrap_or(SubscriptionKind::Direct);

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
        subscription: String,
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
        let subscription_label =
            crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription)
                .to_string();
        let group = (subscription_label, entry.model.clone());
        if current_group.as_ref() != Some(&group) {
            lines.push(ProvidersLine::GroupHeader {
                subscription: group.0.clone(),
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

/// Compact one-line render of a provider list entry. Group headers and the
/// trailing "Add provider" action use distinct visual treatments so the
/// flat row stream still scans as a hierarchy.
pub(crate) fn format_line(line: &ProvidersLine, focused: bool, _width: usize) -> Line<'static> {
    match line {
        ProvidersLine::GroupHeader { subscription, model } => {
            // Indent + dim, like a tree-section heading. The chevron makes it
            // visually distinct from provider rows.
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "▾ ".to_string(),
                    Style::default().fg(COLOR_DIM).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} · {}", subscription, model),
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

            // ✓/✗ enabled state: green check when on, dim cross when off.
            let enabled_glyph = if entry.enabled {
                Span::styled("✓".to_string(), Style::default().fg(COLOR_OK))
            } else {
                Span::styled("✗".to_string(), Style::default().fg(COLOR_DIM))
            };

            let cli_style = if focused {
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

            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::raw("  "));
            spans.push(focus_glyph);
            spans.push(Span::raw(" "));
            spans.push(enabled_glyph);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(entry.cli.as_str().to_string(), cli_style));
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(entry.launch_name.clone(), cli_style));
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
            // Eligibility chips (compact, dim by default; brighter when on)
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
