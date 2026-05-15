use super::pad_right;
#[cfg(test)]
pub(crate) use crate::app_runtime::views::config_panel::providers::all_vendors;
pub(crate) use crate::app_runtime::views::config_panel::providers::{
    AddProviderField, ProvidersEditor, ProvidersLine, get_lines,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const COLOR_FOCUS: Color = Color::Cyan;
const COLOR_SUBSCRIPTION: Color = Color::Magenta;
const COLOR_DIM: Color = Color::Gray;
const COLOR_OVERRIDE: Color = Color::Yellow;
const COLOR_OK: Color = Color::Green;
const COLOR_FREE: Color = Color::Cyan;
const COLOR_CHEAP: Color = Color::Blue;
const COLOR_TOUGH: Color = Color::Red;
const COLOR_EFFORT: Color = Color::Yellow;

const SUB_COL_WIDTH: usize = 11;
const CLI_COL_WIDTH: usize = 8;

pub(crate) fn format_line(line: &ProvidersLine, focused: bool, _width: usize) -> Line<'static> {
    match line {
        ProvidersLine::VendorHeader { vendor, folded } => {
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

            let custom_chip = if *is_baked { None } else { Some("custom") };
            let subscription_label =
                crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription);

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

            let mut chips: Vec<(String, Color)> = Vec::new();
            if let Some(label) = custom_chip {
                chips.push((label.to_string(), COLOR_OVERRIDE));
            }
            if entry.free {
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
            let style = if focused {
                Style::default()
                    .fg(COLOR_FOCUS)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLOR_DIM)
            };
            Line::from(vec![
                Span::raw(" "),
                focus_span(focused),
                Span::raw(" "),
                Span::styled("+ New model provider".to_string(), style),
            ])
        }
    }
}

pub(crate) fn focus_span(focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            "▌".to_string(),
            Style::default()
                .fg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    }
}
