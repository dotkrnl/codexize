//! Responsive models area: full-table, compact-quota, or omitted, chosen
//! from the terminal height with hysteresis. Pure render given prev-mode.
//!
//! The renderer is the source of truth for the spec's mode-selection rule:
//! callers pass the terminal height and the renderer derives
//! `models_budget = term_h - 11` (1 each for top rule, bottom rule, keymap;
//! 8 for pipeline body floor — see spec §"Mode-selection rule"). Keeping the
//! `- 11` here means a single change site if the chrome reservation ever
//! shifts.

use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::model_names;
use crate::selection::{
    CachedModel, QuotaError, VendorKind,
    config::SelectionPhase,
    display::{phase_rank, visible_models},
    ranking::{VersionIndex, selection_probability},
};

use super::models::{vendor_color, vendor_prefix};
use super::models_area_view_model::{
    ProbColumn, QuotaColumn, RESET_TIME_MAX_WIDTH, ResetColumn, VERY_WIDE_THRESHOLD, choose_mode,
    format_name_with_freshness, name_budget_for, name_width_min, probability_color,
    probability_percent,
};

pub use super::models_area_view_model::ModelsAreaMode;

/// Lines reserved by surrounding chrome before the models area:
/// top rule (1) + bottom rule (1) + keymap (1) + pipeline body floor (8).
/// `models_budget = term_h - CHROME_RESERVED_LINES`.
pub const CHROME_RESERVED_LINES: u16 = 11;
/// Preserve the long-standing models-area compact cutoff. The split view uses
/// a separate full-height threshold, so centralizing that value must not
/// silently change existing models-area behavior.
const RESPONSIVE_MODELS_AREA_THRESHOLD: u16 = 50;

/// Pure renderer.
///
/// `term_h` is the full terminal height; the renderer derives
/// `models_budget = term_h - 11` itself. Returns the rendered lines (empty
/// when the budget is below 1 line, the terminal is too narrow, or no
/// models are visible) plus the chosen mode. The mode must be persisted by
/// the caller and passed back as `prev_mode` next frame to honor the
/// hysteresis described in the spec: full→compact at the strict
/// `models_budget < visible_count` threshold, compact→full only when
/// `models_budget >= visible_count + 1`.
pub fn responsive_models_area(
    models: &[CachedModel],
    versions: &VersionIndex,
    quota_errors: &[QuotaError],
    width: u16,
    term_h: u16,
    prev_mode: ModelsAreaMode,
) -> (Vec<Line<'static>>, ModelsAreaMode) {
    let visible = visible_models(models, versions);
    let visible_count = visible.len() as u16;
    let models_budget = term_h.saturating_sub(CHROME_RESERVED_LINES);

    // Spec §"Mode-selection rule": "If even 1 line is unavailable, omit the
    // models region entirely". Preserve prev_mode so a transient small
    // terminal does not reset the hysteresis state when it grows back.
    if visible_count == 0 || width == 0 || models_budget == 0 {
        return (Vec::new(), prev_mode);
    }

    if term_h < RESPONSIVE_MODELS_AREA_THRESHOLD {
        let lines = render_compact_quota(models, quota_errors, width);
        return (lines, ModelsAreaMode::CompactQuota);
    }

    let mode = choose_mode(visible_count, models_budget, prev_mode);

    let lines = match mode {
        ModelsAreaMode::FullTable => render_full_table(models, versions, quota_errors, width),
        ModelsAreaMode::CompactQuota => render_compact_quota(models, quota_errors, width),
    };

    (lines, mode)
}

// ---------------------------------------------------------------------------
// Width tiers
// ---------------------------------------------------------------------------

/// Empirically choose a layout: try each from widest to narrowest.
/// First pass: try to fit without truncating the full name (`max_req_name_width`).
/// Second pass: if must truncate, pick the first layout leaving >= `NAME_WIDTH_MIN`.
fn choose_layout(
    width: u16,
    vendor_width: usize,
    max_req_name_width: usize,
) -> (QuotaColumn, ProbColumn, ResetColumn) {
    let layouts = [
        (QuotaColumn::Expanded, ProbColumn::IpbrVerbose),
        (QuotaColumn::Narrow, ProbColumn::IpbrVerbose),
        (QuotaColumn::Expanded, ProbColumn::Ipbr),
        (QuotaColumn::Narrow, ProbColumn::Ipbr),
        (QuotaColumn::Expanded, ProbColumn::TopRank),
        (QuotaColumn::Narrow, ProbColumn::TopRank),
        (QuotaColumn::Expanded, ProbColumn::None),
        (QuotaColumn::Narrow, ProbColumn::None),
    ];

    if width >= VERY_WIDE_THRESHOLD {
        for &(quota, prob) in &layouts {
            if name_budget_for(width, vendor_width, quota, prob, ResetColumn::Shown)
                >= max_req_name_width
            {
                return (quota, prob, ResetColumn::Shown);
            }
        }
    }

    for &(quota, prob) in &layouts {
        if name_budget_for(width, vendor_width, quota, prob, ResetColumn::Hidden)
            >= max_req_name_width
        {
            return (quota, prob, ResetColumn::Hidden);
        }
    }

    if width >= VERY_WIDE_THRESHOLD {
        for &(quota, prob) in &layouts {
            if name_budget_for(width, vendor_width, quota, prob, ResetColumn::Shown)
                >= name_width_min()
            {
                return (quota, prob, ResetColumn::Shown);
            }
        }
    }

    for &(quota, prob) in &layouts {
        if name_budget_for(width, vendor_width, quota, prob, ResetColumn::Hidden)
            >= name_width_min()
        {
            return (quota, prob, ResetColumn::Hidden);
        }
    }

    (QuotaColumn::Narrow, ProbColumn::None, ResetColumn::Hidden)
}

fn render_full_table(
    models: &[CachedModel],
    versions: &VersionIndex,
    quota_errors: &[QuotaError],
    width: u16,
) -> Vec<Line<'static>> {
    let visible_set = visible_models(models, versions);

    let max_req_name_width = models
        .iter()
        .filter(|m| visible_set.contains(&m.name))
        .map(|model| {
            let prefix = vendor_prefix(model.vendor);
            let short_name = model_names::display_name_for_vendor(&model.name, prefix);
            let mut w = short_name.width();
            if model.fallback_from.is_some() {
                w += 6; // " (new)"
            }
            w
        })
        .max()
        .unwrap_or(0);

    let vendor_width = vendor_column_width();
    let (quota_col, prob_col, reset_col) = choose_layout(width, vendor_width, max_req_name_width);
    let name_width =
        name_budget_for(width, vendor_width, quota_col, prob_col, reset_col).max(name_width_min());

    // Probabilities are normalised against the global total over every
    // assembled model (not just the visible subset) so that filtering does
    // not artificially inflate percentages.
    let prob_for = |phase: SelectionPhase, model: &CachedModel| -> f64 {
        selection_probability(model, phase, versions)
    };
    let total_for =
        |phase: SelectionPhase| -> f64 { models.iter().map(|m| prob_for(phase, m)).sum() };

    let total_idea = total_for(SelectionPhase::Idea);
    let total_planning = total_for(SelectionPhase::Planning);
    let total_build = total_for(SelectionPhase::Build);
    let total_review = total_for(SelectionPhase::Review);

    let idea_ranks = phase_rank(models, SelectionPhase::Idea, versions);
    let planning_ranks = phase_rank(models, SelectionPhase::Planning, versions);
    let build_ranks = phase_rank(models, SelectionPhase::Build, versions);
    let review_ranks = phase_rank(models, SelectionPhase::Review, versions);

    let max_for = |totals: f64, phase: SelectionPhase| -> u8 {
        models
            .iter()
            .map(|m| probability_percent(prob_for(phase, m), totals))
            .max()
            .unwrap_or(0)
    };
    let max_idea = max_for(total_idea, SelectionPhase::Idea);
    let max_planning = max_for(total_planning, SelectionPhase::Planning);
    let max_build = max_for(total_build, SelectionPhase::Build);
    let max_review = max_for(total_review, SelectionPhase::Review);

    let mut visible_models_list: Vec<&CachedModel> = models
        .iter()
        .filter(|m| visible_set.contains(&m.name))
        .collect();

    // Sort globally by Build score descending, tie-break vendor then name
    visible_models_list.sort_by(|a, b| {
        let prob_a = prob_for(SelectionPhase::Build, a);
        let prob_b = prob_for(SelectionPhase::Build, b);
        prob_b
            .partial_cmp(&prob_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.vendor.cmp(&b.vendor))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut lines: Vec<Line<'static>> = Vec::new();
    for model in visible_models_list {
        let label = vendor_label(model.vendor);
        let color = vendor_color(model.vendor);
        let prefix = vendor_prefix(model.vendor);

        let short_name = model_names::display_name_for_vendor(&model.name, prefix);
        let vendor_failed = quota_errors.iter().any(|err| err.vendor == model.vendor);

        let vendor_span = Span::styled(
            format!("{label:<vendor_width$}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        );

        let dot_color = if vendor_failed {
            Color::Red
        } else {
            match model.quota_percent {
                Some(v) if v >= 80 => probability_color(v, 100),
                Some(v) if v >= 40 => Color::Yellow,
                Some(0) => Color::Red,
                Some(v) => probability_color(v, 100),
                None => Color::DarkGray,
            }
        };
        let dot_span = Span::styled(STATUS_DOT, Style::default().fg(dot_color));

        let (quota_text, quota_color) = if vendor_failed {
            match quota_col {
                QuotaColumn::Expanded => ("Quota --% ".to_string(), Color::Red),
                QuotaColumn::Narrow => (" --%".to_string(), Color::Red),
            }
        } else {
            match model.quota_percent {
                Some(v) => match quota_col {
                    QuotaColumn::Expanded => {
                        (format!("Quota {:>3}%", v), probability_color(v, 100))
                    }
                    QuotaColumn::Narrow => (format!("{:>3}%", v), probability_color(v, 100)),
                },
                None => match quota_col {
                    QuotaColumn::Expanded => ("Quota --% ".to_string(), Color::DarkGray),
                    QuotaColumn::Narrow => (" --%".to_string(), Color::DarkGray),
                },
            }
        };
        let quota_span = Span::styled(quota_text, Style::default().fg(quota_color));

        let mut spans: Vec<Span<'static>> = vec![
            vendor_span,
            Span::raw(" "),
            dot_span,
            Span::raw(" "),
            quota_span,
            Span::raw(" "),
        ];

        spans.extend(format_name_with_freshness(
            &short_name,
            model.fallback_from.is_some(),
            name_width,
        ));

        match prob_col {
            ProbColumn::IpbrVerbose => {
                let idea_pct =
                    probability_percent(prob_for(SelectionPhase::Idea, model), total_idea);
                let planning_pct =
                    probability_percent(prob_for(SelectionPhase::Planning, model), total_planning);
                let build_pct =
                    probability_percent(prob_for(SelectionPhase::Build, model), total_build);
                let review_pct =
                    probability_percent(prob_for(SelectionPhase::Review, model), total_review);

                spans.push(Span::raw(" "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("Idea ")
                } else {
                    probability_span(
                        "Idea ",
                        idea_pct,
                        max_idea,
                        idea_ranks.get(&model.name) == Some(&1),
                    )
                });
                spans.push(Span::raw("   "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("Plan ")
                } else {
                    probability_span(
                        "Plan ",
                        planning_pct,
                        max_planning,
                        planning_ranks.get(&model.name) == Some(&1),
                    )
                });
                spans.push(Span::raw("   "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("Build ")
                } else {
                    probability_span(
                        "Build ",
                        build_pct,
                        max_build,
                        build_ranks.get(&model.name) == Some(&1),
                    )
                });
                spans.push(Span::raw("   "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("Review ")
                } else {
                    probability_span(
                        "Review ",
                        review_pct,
                        max_review,
                        review_ranks.get(&model.name) == Some(&1),
                    )
                });
            }
            ProbColumn::Ipbr => {
                let idea_pct =
                    probability_percent(prob_for(SelectionPhase::Idea, model), total_idea);
                let planning_pct =
                    probability_percent(prob_for(SelectionPhase::Planning, model), total_planning);
                let build_pct =
                    probability_percent(prob_for(SelectionPhase::Build, model), total_build);
                let review_pct =
                    probability_percent(prob_for(SelectionPhase::Review, model), total_review);

                spans.push(Span::raw(" "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("I")
                } else {
                    probability_span(
                        "I",
                        idea_pct,
                        max_idea,
                        idea_ranks.get(&model.name) == Some(&1),
                    )
                });
                spans.push(Span::raw(" "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("P")
                } else {
                    probability_span(
                        "P",
                        planning_pct,
                        max_planning,
                        planning_ranks.get(&model.name) == Some(&1),
                    )
                });
                spans.push(Span::raw(" "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("B")
                } else {
                    probability_span(
                        "B",
                        build_pct,
                        max_build,
                        build_ranks.get(&model.name) == Some(&1),
                    )
                });
                spans.push(Span::raw(" "));
                spans.push(if vendor_failed {
                    probability_unavailable_span("R")
                } else {
                    probability_span(
                        "R",
                        review_pct,
                        max_review,
                        review_ranks.get(&model.name) == Some(&1),
                    )
                });
            }
            ProbColumn::TopRank => {
                spans.push(Span::raw(" "));
                spans.push(top_rank_prob_span(
                    model,
                    vendor_failed,
                    prob_for(SelectionPhase::Idea, model),
                    prob_for(SelectionPhase::Planning, model),
                    prob_for(SelectionPhase::Build, model),
                    prob_for(SelectionPhase::Review, model),
                    total_idea,
                    total_planning,
                    total_build,
                    total_review,
                    max_idea,
                    max_planning,
                    max_build,
                    max_review,
                    idea_ranks.get(&model.name) == Some(&1),
                    planning_ranks.get(&model.name) == Some(&1),
                    build_ranks.get(&model.name) == Some(&1),
                    review_ranks.get(&model.name) == Some(&1),
                ));
            }
            ProbColumn::None => {}
        }

        if let ResetColumn::Shown = reset_col {
            spans.push(Span::raw(" "));
            if let Some(quota_resets_at) = model.quota_resets_at {
                let text = format_reset_time(quota_resets_at);
                let pad = RESET_TIME_MAX_WIDTH.saturating_sub(text.width());
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                }
                spans.push(Span::styled(text, Style::default().fg(Color::DarkGray)));
            } else {
                // Keep the column reserved so rows stay aligned even when only
                // some providers currently expose reset timestamps.
                spans.push(Span::raw(" ".repeat(RESET_TIME_MAX_WIDTH)));
            }
        }

        lines.push(Line::from(spans));
    }

    lines
}

fn format_reset_time(dt: DateTime<Utc>) -> String {
    let dur = dt.signed_duration_since(Utc::now());
    if dur.num_seconds() <= 0 {
        return "expired".to_string();
    }

    let days = dur.num_days();
    let hours = dur.num_hours() % 24;
    let mins = dur.num_minutes() % 60;
    let text = if days > 0 {
        format!("in {days}d {hours}h")
    } else if hours > 0 {
        format!("in {hours}h {mins}m")
    } else {
        format!("in {mins}m")
    };

    if text.width() <= RESET_TIME_MAX_WIDTH {
        return text;
    }

    let mut truncated = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > RESET_TIME_MAX_WIDTH {
            break;
        }
        truncated.push(ch);
        width += ch_width;
    }
    truncated
}

// ---------------------------------------------------------------------------
// Probability column helpers
// ---------------------------------------------------------------------------

fn probability_span(label: &str, pct: u8, max_pct: u8, is_top_rank: bool) -> Span<'static> {
    let mut style = Style::default().fg(probability_color(pct, max_pct));
    if is_top_rank {
        style = style.add_modifier(Modifier::BOLD);
    }
    Span::styled(format!("{label}{pct:>2}"), style)
}

fn probability_unavailable_span(label: &str) -> Span<'static> {
    Span::styled(format!("{label}--"), Style::default().fg(Color::Red))
}

// ---------------------------------------------------------------------------
// Full table mode
// ---------------------------------------------------------------------------

const STATUS_DOT: &str = "●";

fn vendor_label(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Claude => "claude",
        VendorKind::Codex => "codex",
        VendorKind::Gemini => "gemini",
        VendorKind::Kimi => "kimi",
    }
}

/// Width of the vendor-tag column (padded).
fn vendor_column_width() -> usize {
    6
}

#[allow(clippy::too_many_arguments)]
fn top_rank_prob_span(
    _model: &CachedModel,
    vendor_failed: bool,
    p_idea: f64,
    p_plan: f64,
    p_build: f64,
    p_review: f64,
    total_idea: f64,
    total_plan: f64,
    total_build: f64,
    total_review: f64,
    max_idea: u8,
    max_plan: u8,
    max_build: u8,
    max_review: u8,
    rank1_idea: bool,
    rank1_plan: bool,
    rank1_build: bool,
    rank1_review: bool,
) -> Span<'static> {
    if vendor_failed {
        // Prefer the "P" letter when probabilities are unavailable so the
        // column reads as a single 3-col `P--` rather than guessing a phase.
        return probability_unavailable_span("P");
    }

    // Pick the phase where this row is rank-1; that is the cell shown.
    // If the row is not rank-1 anywhere, pick the phase with the highest
    // percentage so the column still carries a useful signal.
    let candidates: [(bool, &str, u8, u8, bool); 4] = [
        (
            rank1_idea,
            "I",
            probability_percent(p_idea, total_idea),
            max_idea,
            true,
        ),
        (
            rank1_plan,
            "P",
            probability_percent(p_plan, total_plan),
            max_plan,
            true,
        ),
        (
            rank1_build,
            "B",
            probability_percent(p_build, total_build),
            max_build,
            true,
        ),
        (
            rank1_review,
            "R",
            probability_percent(p_review, total_review),
            max_review,
            true,
        ),
    ];

    if let Some((_, label, pct, max, _)) = candidates.iter().find(|c| c.0) {
        return probability_span(label, *pct, *max, true);
    }

    // No rank-1 phase: surface the row's strongest cell, unbolded.
    let (label, pct, max) = candidates
        .iter()
        .map(|c| (c.1, c.2, c.3))
        .max_by_key(|(_, pct, _)| *pct)
        .unwrap_or(("P", 0, 0));
    probability_span(label, pct, max, false)
}

// ---------------------------------------------------------------------------
// Compact quota line
// ---------------------------------------------------------------------------

const COMPACT_OMIT_BELOW: u16 = 40;

fn render_compact_quota(
    models: &[CachedModel],
    quota_errors: &[QuotaError],
    width: u16,
) -> Vec<Line<'static>> {
    if width < COMPACT_OMIT_BELOW {
        return Vec::new();
    }

    let order = [
        VendorKind::Kimi,
        VendorKind::Claude,
        VendorKind::Codex,
        VendorKind::Gemini,
    ];

    let mut vendors_to_render = Vec::new();
    for vendor in order {
        if let Some(model) = models.iter().filter(|m| m.vendor == vendor).max_by(|a, b| {
            a.current_score
                .partial_cmp(&b.current_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            vendors_to_render.push((vendor, model));
        }
    }

    let mut expanded_width = 0;
    for (i, (vendor, model)) in vendors_to_render.iter().enumerate() {
        if i > 0 {
            expanded_width += 3; // " · "
        }
        let vendor_failed = quota_errors.iter().any(|err| err.vendor == *vendor);
        let label = vendor_label(*vendor);
        let quota_str_len = if vendor_failed {
            2 // "--"
        } else {
            match model.quota_percent {
                Some(v) => format!("{v}%").width(),
                None => 2,
            }
        };
        expanded_width += label.width() + 1 + 6 + quota_str_len; // label + sp + "Quota " + quota
    }

    let use_expanded_quota = expanded_width <= width as usize;

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut first = true;
    for (vendor, model) in vendors_to_render {
        let vendor_failed = quota_errors.iter().any(|err| err.vendor == vendor);

        if !first {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        first = false;

        let label = vendor_label(vendor);
        spans.push(Span::styled(
            label.to_string(),
            Style::default().fg(vendor_color(vendor)),
        ));
        spans.push(Span::raw(" "));
        if use_expanded_quota {
            spans.push(Span::styled("Quota ", Style::default().fg(Color::DarkGray)));
        }

        if vendor_failed {
            spans.push(Span::styled("--", Style::default().fg(Color::Red)));
        } else {
            match model.quota_percent {
                Some(v) => {
                    let style = Style::default().fg(probability_color(v, 100));
                    spans.push(Span::styled(format!("{v}%"), style));
                }
                None => {
                    spans.push(Span::styled("--", Style::default().fg(Color::DarkGray)));
                }
            }
        }
    }

    if spans.is_empty() {
        Vec::new()
    } else {
        vec![Line::from(spans)]
    }
}

#[cfg(test)]
mod tests_mod;
