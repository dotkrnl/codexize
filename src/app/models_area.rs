//! Responsive models area: full-table, compact-quota, or omitted, chosen
//! from the terminal height with hysteresis. Pure render given prev-mode.
//!
//! The renderer is the source of truth for the spec's mode-selection rule:
//! callers pass the terminal height and the renderer derives
//! `models_budget = term_h - 11` (1 each for top rule, bottom rule, keymap;
//! 8 for pipeline body floor — see spec §"Mode-selection rule"). Keeping the
//! `- 11` here means a single change site if the chrome reservation ever
//! shifts.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::model_names;
use crate::selection::{
    CachedModel, QuotaError, VendorKind,
    config::SelectionPhase,
    display::{phase_rank, visible_models},
    ranking::{VersionIndex, selection_probability},
};

use super::models::{vendor_color, vendor_prefix};

/// Mode chosen by the height budget. Owned by `App` and threaded back in
/// next frame as `prev_mode` so the asymmetric hysteresis thresholds work.
///
/// The default is `FullTable`: on the first frame the strict (== count)
/// threshold then applies, matching the spec's "no hysteresis on entry".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelsAreaMode {
    #[default]
    FullTable,
    CompactQuota,
}

/// Lines reserved by surrounding chrome before the models area:
/// top rule (1) + bottom rule (1) + keymap (1) + pipeline body floor (8).
/// `models_budget = term_h - CHROME_RESERVED_LINES`.
pub const CHROME_RESERVED_LINES: u16 = 11;

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

    if term_h < 50 {
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

fn choose_mode(
    visible_count: u16,
    models_budget: u16,
    prev_mode: ModelsAreaMode,
) -> ModelsAreaMode {
    match prev_mode {
        ModelsAreaMode::FullTable => {
            // Full → Compact requires one extra row of headroom; Compact → Full below
            // keeps the same threshold, creating the intended two-line hysteresis band.
            if models_budget > visible_count {
                ModelsAreaMode::FullTable
            } else {
                ModelsAreaMode::CompactQuota
            }
        }
        ModelsAreaMode::CompactQuota => {
            // Compact → Full requires +1 to absorb single-row resize jitter.
            if models_budget >= visible_count.saturating_add(1) {
                ModelsAreaMode::FullTable
            } else {
                ModelsAreaMode::CompactQuota
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Width tiers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaColumn {
    Expanded, // "Quota 100%" — 10 cols
    Narrow,   // "100%"       —  4 cols
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbColumn {
    IpbrVerbose, // "Idea XX   Plan XX   Build XX   Review XX" — 40 cols
    Ipbr,        // full "Ixx Pxx Bxx Rxx"                      — 15 cols
    TopRank,     // single 3-col cell, top-rank phase          —  3 cols
    None,
}

/// Compute the name budget if we selected `quota`, `prob_col` and `vendor_width`.
fn name_budget_for(
    width: u16,
    vendor_width: usize,
    quota: QuotaColumn,
    prob_col: ProbColumn,
) -> usize {
    let fixed = full_row_fixed_width(vendor_width, quota, prob_col);
    (width as usize).saturating_sub(fixed)
}

/// Empirically choose a layout: try each from widest to narrowest.
/// First pass: try to fit without truncating the full name (`max_req_name_width`).
/// Second pass: if must truncate, pick the first layout leaving >= `NAME_WIDTH_MIN`.
fn choose_layout(
    width: u16,
    vendor_width: usize,
    max_req_name_width: usize,
) -> (QuotaColumn, ProbColumn) {
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

    for &(quota, prob) in &layouts {
        if name_budget_for(width, vendor_width, quota, prob) >= max_req_name_width {
            return (quota, prob);
        }
    }

    for &(quota, prob) in &layouts {
        if name_budget_for(width, vendor_width, quota, prob) >= NAME_WIDTH_MIN {
            return (quota, prob);
        }
    }

    (QuotaColumn::Narrow, ProbColumn::None)
}

// ---------------------------------------------------------------------------
// Probability column helpers
// ---------------------------------------------------------------------------

fn probability_percent(weight: f64, total: f64) -> u8 {
    if total <= 0.0 || weight <= 0.0 {
        return 0;
    }
    (weight / total * 100.0).round().clamp(0.0, 99.0) as u8
}

fn probability_color(pct: u8, max_pct: u8) -> Color {
    if pct == 0 {
        return Color::DarkGray;
    }
    let t = if max_pct == 0 {
        0.0
    } else {
        (pct as f64 / max_pct as f64).clamp(0.0, 1.0)
    };
    let (r, g) = if t < 0.5 {
        let k = t / 0.5;
        (220, (40.0 + (220.0 - 40.0) * k) as u8)
    } else {
        let k = (t - 0.5) / 0.5;
        ((220.0 - (220.0 - 60.0) * k) as u8, 220)
    };
    Color::Rgb(r, g, 60)
}

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

const NAME_WIDTH_MIN: usize = 8;

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

/// Width consumed by everything except the model-name column.
///
/// Layout: `{vendor:vw} {dot} {quota:>4} {name + freshness} {probs}`
///
/// Counting separators (one space between each visible column):
///   vw + 1 + 1 + 1 + 4 + 1 + probs_width + (1 if probs)
fn full_row_fixed_width(vendor_width: usize, quota: QuotaColumn, prob_col: ProbColumn) -> usize {
    let probs = match prob_col {
        ProbColumn::IpbrVerbose => 40,
        ProbColumn::Ipbr => 15,
        ProbColumn::TopRank => 3,
        ProbColumn::None => 0,
    };
    let prob_separator = if probs == 0 { 0 } else { 1 };
    let quota_width = match quota {
        QuotaColumn::Expanded => 10,
        QuotaColumn::Narrow => 4,
    };
    // vendor + sp + dot + sp + quota + sp + name (variable) + prob_sep + probs
    vendor_width + 1 + 1 + 1 + quota_width + 1 + prob_separator + probs
}

fn name_budget(width: u16, vendor_width: usize, quota: QuotaColumn, prob_col: ProbColumn) -> usize {
    let fixed = full_row_fixed_width(vendor_width, quota, prob_col);
    let raw = (width as usize).saturating_sub(fixed);
    raw.max(NAME_WIDTH_MIN) // no upper clamp; fills remaining space
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
            let mut w = short_name.chars().count();
            if model.fallback_from.is_some() {
                w += 6; // " (new)"
            }
            w
        })
        .max()
        .unwrap_or(0);

    let vendor_width = vendor_column_width();
    let (quota_col, prob_col) = choose_layout(width, vendor_width, max_req_name_width);
    let name_width = name_budget(width, vendor_width, quota_col, prob_col);

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

        lines.push(Line::from(spans));
    }

    lines
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
// Name + freshness formatting (spec: degrade " (new)" → "*" → omitted)
// ---------------------------------------------------------------------------

fn name_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn freshness_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn ellipsis_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

const ELLIPSIS: &str = "...";

/// Render the `name + freshness` column as a sequence of spans whose total
/// visible width equals `budget`. Freshness degrades " (new)" → "*" →
/// omitted before the name itself starts truncating with an ellipsis.
fn format_name_with_freshness(short_name: &str, is_new: bool, budget: usize) -> Vec<Span<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    let name_len = short_name.chars().count();

    if is_new {
        // 1. " (new)" suffix
        if name_len + " (new)".chars().count() <= budget {
            let mut spans = vec![Span::styled(short_name.to_string(), name_style())];
            spans.push(Span::styled(" (new)", freshness_style()));
            let pad = budget - name_len - " (new)".chars().count();
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            return spans;
        }
        // 2. "*" suffix
        if name_len < budget {
            let mut spans = vec![Span::styled(short_name.to_string(), name_style())];
            spans.push(Span::styled("*", freshness_style()));
            let pad = budget - name_len - 1;
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            return spans;
        }
        // 3. fall through — name truncates with ellipsis, no marker.
    }

    // Plain name path (or freshness fully degraded away).
    if name_len <= budget {
        let pad = budget - name_len;
        let mut spans = vec![Span::styled(short_name.to_string(), name_style())];
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        return spans;
    }

    let ellipsis_len = ELLIPSIS.chars().count();
    if budget > ellipsis_len {
        let visible_chars = budget - ellipsis_len;
        let truncated: String = short_name.chars().take(visible_chars).collect();
        return vec![
            Span::styled(truncated, name_style()),
            Span::styled(ELLIPSIS, ellipsis_style()),
        ];
    }
    vec![Span::styled(
        ELLIPSIS.chars().take(budget).collect::<String>(),
        ellipsis_style(),
    )]
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
                Some(v) => format!("{v}%").chars().count(),
                None => 2,
            }
        };
        expanded_width += label.chars().count() + 1 + 6 + quota_str_len; // label + sp + "Quota " + quota
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
mod tests {
    use super::*;
    use crate::selection::ranking::build_version_index;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget};

    // ----- fixtures -----

    fn model_with_axis_score(name: &str, axis_score: f64, display_order: usize) -> CachedModel {
        CachedModel {
            vendor: VendorKind::Codex,
            name: name.to_string(),
            overall_score: axis_score,
            current_score: 99.0,
            standard_error: 0.0,
            axes: vec![
                ("codequality".to_string(), axis_score),
                ("correctness".to_string(), axis_score),
                ("debugging".to_string(), axis_score),
                ("safety".to_string(), axis_score),
                ("complexity".to_string(), axis_score),
                ("edgecases".to_string(), axis_score),
                ("contextawareness".to_string(), axis_score),
                ("taskcompletion".to_string(), axis_score),
                ("stability".to_string(), axis_score),
            ],
            axis_provenance: std::collections::BTreeMap::new(),
            quota_percent: Some(100),
            display_order,
            fallback_from: None,
        }
    }

    fn vendor_model_with_axis_score(
        vendor: VendorKind,
        name: &str,
        axis_score: f64,
        display_order: usize,
    ) -> CachedModel {
        let mut model = model_with_axis_score(name, axis_score, display_order);
        model.vendor = vendor;
        model
    }

    fn render_to_text(lines: &[Line<'static>], width: u16) -> Vec<String> {
        let height = lines.len().max(1) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines.to_vec()).render(area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect()
    }

    fn full_buffer_line(lines: &[Line<'static>], y: usize, width: u16) -> String {
        render_to_text(lines, width)
            .into_iter()
            .nth(y)
            .unwrap_or_default()
    }

    /// Convert a desired models budget (lines available *for the area*) to a
    /// terminal height the renderer accepts. Mirrors `term_h - 11` in the
    /// renderer so tests read in the same units the spec uses.
    fn term_h_for_budget(budget: u16) -> u16 {
        budget + CHROME_RESERVED_LINES
    }

    // ----- mode-selection / hysteresis -----

    #[test]
    fn empty_models_returns_empty_lines() {
        let models: Vec<CachedModel> = Vec::new();
        let versions = build_version_index(&models);
        let (lines, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(20),
            ModelsAreaMode::FullTable,
        );
        assert!(lines.is_empty());
        assert_eq!(mode, ModelsAreaMode::FullTable);
    }

    #[test]
    fn term_h_below_floor_returns_empty_preserving_prev_mode() {
        // term_h <= CHROME_RESERVED_LINES → models_budget == 0 → omit area.
        let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
        let versions = build_version_index(&models);

        for term_h in [0u16, 5, CHROME_RESERVED_LINES] {
            let (lines, mode) = responsive_models_area(
                &models,
                &versions,
                &[],
                120,
                term_h,
                ModelsAreaMode::CompactQuota,
            );
            assert!(lines.is_empty(), "term_h={term_h}");
            // Preserved across transient small terminals so the area does not
            // flicker mode when the terminal grows back.
            assert_eq!(mode, ModelsAreaMode::CompactQuota, "term_h={term_h}");
        }
    }

    #[test]
    fn full_to_compact_uses_strict_threshold() {
        let models = vec![
            model_with_axis_score("gpt-a", 1.0, 0),
            model_with_axis_score("gpt-b", 1.0, 1),
            model_with_axis_score("gpt-c", 1.0, 2),
        ];
        let versions = build_version_index(&models);
        // visible_count >= 3 (all three picked because per-vendor backfill
        // promotes the best-score representative when phases miss).
        let visible_count = visible_models(&models, &versions).len() as u16;

        // Full mode now needs one row of headroom before it stays full.
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(visible_count),
            ModelsAreaMode::FullTable,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        // One extra row is still below the fixed 50-row compact threshold.
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(visible_count + 1),
            ModelsAreaMode::FullTable,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        // At 50 rows, normal hysteresis applies again and this fixture has
        // enough budget to stay full.
        let (_, mode) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        assert_eq!(mode, ModelsAreaMode::FullTable);
    }

    #[test]
    fn compact_to_full_requires_extra_line() {
        let models = vec![
            model_with_axis_score("gpt-a", 1.0, 0),
            model_with_axis_score("gpt-b", 1.0, 1),
            model_with_axis_score("gpt-c", 1.0, 2),
        ];
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;

        // From compact, models_budget == count is NOT enough (hysteresis +1).
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(visible_count),
            ModelsAreaMode::CompactQuota,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        // models_budget == count + 1 would unlock the switch back under the
        // hysteresis rule, but the sub-50 terminal threshold still wins.
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(visible_count + 1),
            ModelsAreaMode::CompactQuota,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        // At 50 rows, normal hysteresis applies again and the larger budget
        // switches compact mode back to full.
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            50,
            ModelsAreaMode::CompactQuota,
        );
        assert_eq!(mode, ModelsAreaMode::FullTable);
    }

    #[test]
    fn boundary_oscillation_does_not_flip_mode_each_frame() {
        // Spec: "at the boundary, oscillating the height by ±1 across frames
        // must not flip the mode each frame."
        let models = vec![
            model_with_axis_score("gpt-a", 1.0, 0),
            model_with_axis_score("gpt-b", 1.0, 1),
            model_with_axis_score("gpt-c", 1.0, 2),
        ];
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;
        let term_at = term_h_for_budget(visible_count);
        let term_below = term_h_for_budget(visible_count - 1);

        // Start on the boundary in full mode. Full→compact now requires
        // one extra row of headroom, so budget == count drops immediately.
        // From compact the +1 hysteresis means count alone never flips us back.
        let mut mode = ModelsAreaMode::FullTable;

        // Frame 1: budget == count, prev=full → compact.
        let (_, m) = responsive_models_area(&models, &versions, &[], 120, term_at, mode);
        assert_eq!(m, ModelsAreaMode::CompactQuota);
        mode = m;

        for _ in 0..6 {
            let (_, m) = responsive_models_area(&models, &versions, &[], 120, term_at, mode);
            assert_eq!(
                m,
                ModelsAreaMode::CompactQuota,
                "+1 hysteresis must hold compact at boundary"
            );
            mode = m;

            let (_, m) = responsive_models_area(&models, &versions, &[], 120, term_below, mode);
            assert_eq!(m, ModelsAreaMode::CompactQuota);
            mode = m;
        }
    }

    #[test]
    fn omit_then_grow_preserves_compact_state() {
        // Omit must not flip the hysteresis state — when the terminal grows
        // back, prev_mode applies as if the omit never happened.
        let models = vec![
            model_with_axis_score("gpt-a", 1.0, 0),
            model_with_axis_score("gpt-b", 1.0, 1),
            model_with_axis_score("gpt-c", 1.0, 2),
        ];
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;

        let mut mode = ModelsAreaMode::CompactQuota;
        let (_, m) = responsive_models_area(&models, &versions, &[], 120, /*omit*/ 8, mode);
        assert_eq!(m, ModelsAreaMode::CompactQuota, "omit preserves prev_mode");
        mode = m;

        // Grow back exactly to visible_count budget — still inside the +1
        // hysteresis band, so we stay compact even though the strict
        // threshold technically allows full.
        let (_, m) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(visible_count),
            mode,
        );
        assert_eq!(m, ModelsAreaMode::CompactQuota);
    }

    #[test]
    fn mode_default_is_full_table() {
        assert_eq!(ModelsAreaMode::default(), ModelsAreaMode::FullTable);
    }

    // ----- migrated model_strip_* tests -----

    #[test]
    fn full_table_bolds_only_phase_rank_one_when_percentages_round_together() {
        let models = vec![
            model_with_axis_score("gpt-alpha", 1.0, 0),
            model_with_axis_score("gpt-beta", 0.996_655, 1),
        ];
        let versions = build_version_index(&models);

        // Width 60 → Ipbr tier (compact single-letter format).
        let (lines, mode) =
            responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
        assert_eq!(mode, ModelsAreaMode::FullTable);

        // Render to a buffer so we can inspect cell modifiers.
        let area = Rect::new(0, 0, 50, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines.clone()).render(area, &mut buf);

        let beta_y = (0..area.height)
            .find(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, *y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
                    .contains("beta")
            })
            .expect("beta row should be rendered");
        let beta_line: String = (0..area.width)
            .map(|x| buf.cell((x, beta_y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        let build_col = beta_line
            .rfind("B50")
            .expect("beta build probability should round to B50") as u16;
        let build_cell = buf.cell((build_col, beta_y)).expect("build cell");

        assert!(!build_cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn full_table_truncates_long_names_on_narrow_width() {
        let models = vec![model_with_axis_score(
            "gpt-very-long-model-name-that-will-overflow",
            1.0,
            0,
        )];
        let versions = build_version_index(&models);

        // Width 50 → tier 3 (no probabilities, 2-letter vendor) with a tight
        // name budget so the full name cannot fit.
        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 50);

        assert!(
            row.contains("..."),
            "narrow width should truncate name with ellipsis: {row:?}"
        );
        assert!(
            !row.contains("very-long-model-name-that-will-overflow"),
            "full name should not fit: {row:?}"
        );
    }

    #[test]
    fn full_table_drops_probabilities_below_60() {
        // Spec rule: "drop probabilities entirely below ~60 cols". The
        // pre-cutover model_strip used to keep IPBR at width 50 — under the
        // new spec, that whole column collapses.
        let models = vec![model_with_axis_score(
            "gpt-very-long-model-name-that-will-overflow",
            1.0,
            0,
        )];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 50);

        assert!(
            !row.contains("I 0")
                && !row.contains("P 0")
                && !row.contains("B 0")
                && !row.contains("R 0"),
            "probabilities must be dropped below 60: {row:?}"
        );
    }

    #[test]
    fn full_table_keeps_full_ipbr_at_or_above_80() {
        let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 80, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 80);

        // All four phase letters must appear at width 80.
        assert!(
            row.contains('I') && row.contains('P') && row.contains('B') && row.contains('R'),
            "full IPBR at width 80: {row:?}"
        );
    }

    #[test]
    fn full_table_collapses_to_top_rank_only_between_60_and_80() {
        let models = vec![
            model_with_axis_score("gpt-alpha", 100.0, 0),
            model_with_axis_score("gpt-beta", 1.0, 1),
        ];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 70, 50, ModelsAreaMode::FullTable);

        // The full IPBR string would not fit; the top-rank cell does.
        // Each row should contain exactly ONE phase-letter cell (Lxx where
        // L is one of I/P/B/R and xx is two digits). We assert that at
        // least one row carries one such cell, and no row carries the full
        // four-cell IPBR sequence.
        let texts = render_to_text(&lines, 70);
        for (i, row) in texts.iter().enumerate() {
            let cell_count = ["I", "P", "B", "R"]
                .iter()
                .filter(|ph| {
                    let bytes = row.as_bytes();
                    bytes.windows(3).any(|w| {
                        w[0] == ph.as_bytes()[0]
                            && (w[1] as char).is_ascii_digit()
                            && (w[2] as char).is_ascii_digit()
                    }) || bytes.windows(3).any(|w| {
                        w[0] == ph.as_bytes()[0] && w[1] == b' ' && (w[2] as char).is_ascii_digit()
                    })
                })
                .count();
            assert!(
                cell_count <= 1,
                "row {i}: expected at most one phase cell at 60-79: {row:?}"
            );
        }
    }

    #[test]
    fn full_table_truncates_fallback_marker_text_on_narrow_width() {
        // Migrated from model_strip_truncates_fallback_marker_text_on_narrow_width.
        // Under the new spec, the freshness marker DEGRADES rather than
        // truncating: " (new)" → "*" → omitted before the name itself starts
        // ellipsis-truncating. So at narrow widths we expect either "*" or no
        // marker, not "name (...".
        let mut model = model_with_axis_score("gpt-opus-4-1", 1.0, 0);
        model.fallback_from = Some("gpt-4-1".to_string());
        let models = vec![model];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 48, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 48);

        assert!(
            !row.contains("(...") && !row.contains("(n..."),
            "freshness marker must degrade, not partially truncate: {row:?}"
        );
        assert!(
            row.contains("opus-4-1"),
            "name itself should still appear when budget allows it: {row:?}"
        );
    }

    #[test]
    fn full_table_shows_full_name_on_wide_width() {
        let models = vec![model_with_axis_score("gpt-opus-4-5-20251101", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 120);

        assert!(
            row.contains("opus-4-5-20251101"),
            "full short name should appear on wide width: {row:?}"
        );
        assert!(
            !row.contains("..."),
            "should not truncate on wide width: {row:?}"
        );
    }

    #[test]
    fn full_table_uses_gemini_preview_display_label() {
        let models = vec![vendor_model_with_axis_score(
            VendorKind::Gemini,
            "gemini-3.1-pro-preview",
            1.0,
            0,
        )];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 120);

        assert!(
            row.contains("3.1-pro"),
            "short display label should appear: {row:?}"
        );
        assert!(
            !row.contains("3.1-pro-preview"),
            "preview suffix should not appear in display label: {row:?}"
        );
    }

    #[test]
    fn full_table_shows_new_suffix_for_fallback_models_on_wide_width() {
        let mut model = model_with_axis_score("gpt-opus-4-5-20251101", 1.0, 0);
        model.fallback_from = Some("gpt-4-5".to_string());
        let models = vec![model];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 120);

        assert!(
            row.contains("opus-4-5-20251101 (new)"),
            "fallback model should show (new) suffix on wide width: {row:?}"
        );
    }

    #[test]
    fn full_table_omits_provenance_labels() {
        let mut model = model_with_axis_score("gpt-alpha", 1.0, 0);
        model.axis_provenance = std::collections::BTreeMap::from([
            ("correctness".to_string(), "suite:hourly".to_string()),
            ("contextawareness".to_string(), "suite:tooling".to_string()),
        ]);
        let models = vec![model];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 200, 50, ModelsAreaMode::FullTable);
        let combined = render_to_text(&lines, 200).join("\n");

        assert!(
            !combined.contains("suite:hourly") && !combined.contains("suite:tooling"),
            "provenance labels must not render: {combined:?}"
        );
    }

    // ----- name + freshness exact-width contract -----

    #[test]
    fn format_name_with_freshness_exact_width() {
        // Full name fits — padded to target width.
        let spans = format_name_with_freshness("short", false, 10);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 10);

        // Name + " (new)" fits.
        let spans = format_name_with_freshness("short", true, 15);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 15);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "short (new)    ");

        // Freshness degrades to "*" when " (new)" no longer fits.
        let spans = format_name_with_freshness("gpt-4-turbo", true, 13);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 13);
        assert!(
            text.starts_with("gpt-4-turbo*"),
            "freshness should degrade to *: {text:?}"
        );

        // Freshness omitted entirely when even "*" no longer fits.
        let spans = format_name_with_freshness("gpt-4-turbo", true, 11);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 11);
        assert_eq!(text, "gpt-4-turbo", "freshness omitted, name fits exactly");

        // Name truncated with ellipsis (no marker since we are in plain mode).
        let spans = format_name_with_freshness("verylongname", false, 10);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 10);
        assert!(spans.iter().any(|s| s.content.contains("...")));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "verylon...");

        // Very narrow — only ellipsis fits.
        let spans = format_name_with_freshness("x", false, 2);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 2);
    }

    // ----- compact-quota mode -----

    #[test]
    fn compact_quota_renders_per_vendor_entries() {
        let models = vec![
            vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Codex, "gpt-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Gemini, "gemini-1", 50.0, 0),
        ];
        let versions = build_version_index(&models);

        let (lines, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        let row = full_buffer_line(&lines, 0, 120);
        for tag in ["kimi", "claude", "codex", "gemini"] {
            assert!(row.contains(tag), "missing {tag}: {row:?}");
        }
        assert!(
            row.contains("100%"),
            "100% quota should render verbatim: {row:?}"
        );
    }

    #[test]
    fn compact_quota_keeps_full_vendor_names_below_60() {
        let models = vec![
            vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
        ];
        let versions = build_version_index(&models);

        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &[],
            50,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );
        let row = full_buffer_line(&lines, 0, 50);
        assert!(row.contains("kimi"), "full kimi label: {row:?}");
        assert!(row.contains("claude"), "full claude label: {row:?}");
    }

    #[test]
    fn compact_quota_omits_below_40() {
        let models = vec![vendor_model_with_axis_score(
            VendorKind::Kimi,
            "kimi-1",
            50.0,
            0,
        )];
        let versions = build_version_index(&models);

        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &[],
            30,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );
        assert!(lines.is_empty(), "compact line must omit below 40 cols");
    }

    #[test]
    fn compact_quota_failed_fetch_renders_red_dashes() {
        let models = vec![vendor_model_with_axis_score(
            VendorKind::Kimi,
            "kimi-1",
            50.0,
            0,
        )];
        let versions = build_version_index(&models);
        let errors = vec![QuotaError {
            vendor: VendorKind::Kimi,
            message: "boom".to_string(),
        }];

        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &errors,
            120,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );

        // Render to a buffer so we can inspect the red color on the dashes.
        let area = Rect::new(0, 0, 120, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);

        let row: String = (0..area.width)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        let dash_col = row.find("--").expect("`--` must appear for failed quota") as u16;
        let cell = buf.cell((dash_col, 0)).expect("dash cell");
        assert_eq!(cell.fg, Color::Red);
    }

    // ----- failed-vendor full-table styling -----

    #[test]
    fn full_table_failed_vendor_renders_red_dashes_for_quota_and_probs() {
        let models = vec![vendor_model_with_axis_score(
            VendorKind::Kimi,
            "kimi-1",
            50.0,
            0,
        )];
        let versions = build_version_index(&models);
        let errors = vec![QuotaError {
            vendor: VendorKind::Kimi,
            message: "boom".to_string(),
        }];

        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &errors,
            120,
            50,
            ModelsAreaMode::FullTable,
        );

        let area = Rect::new(0, 0, 120, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);
        let row: String = (0..area.width)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();

        // Quota cell shows `--%` in red.
        let quota_col = row.find("--%").expect("--% must appear for failed quota") as u16;
        assert_eq!(buf.cell((quota_col, 0)).unwrap().fg, Color::Red);

        // At width 120, IpbrVerbose is chosen, so phase labels are full words.
        // probability_unavailable_span renders the entire label+dashes as one red span.
        for (label, pat) in [
            ("Idea", "Idea --"),
            ("Plan", "Plan --"),
            ("Build", "Build --"),
            ("Review", "Review --"),
        ] {
            let col = row
                .find(pat)
                .unwrap_or_else(|| panic!("expected {pat:?} for failed vendor ({label}): {row:?}"))
                as u16;
            // Check that the first char of the label is red (the whole span is red).
            assert_eq!(buf.cell((col, 0)).unwrap().fg, Color::Red);
        }
    }

    #[test]
    fn full_table_dot_color_tracks_quota_not_score() {
        let mut high_score_no_quota = model_with_axis_score("gpt-alpha", 1.0, 0);
        high_score_no_quota.current_score = 99.0;
        high_score_no_quota.quota_percent = Some(0);
        let mut low_score_full_quota = model_with_axis_score("gpt-beta", 1.0, 1);
        low_score_full_quota.current_score = 1.0;
        low_score_full_quota.quota_percent = Some(100);
        let models = vec![high_score_no_quota, low_score_full_quota];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);

        let area = Rect::new(0, 0, 120, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);
        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect()
            })
            .collect();

        let red_row = rows
            .iter()
            .position(|row| row.contains("alpha"))
            .expect("alpha row");
        let red_col = rows[red_row].find(STATUS_DOT).expect("alpha dot") as u16;
        assert_eq!(buf.cell((red_col, red_row as u16)).unwrap().fg, Color::Red);

        let green_row = rows
            .iter()
            .position(|row| row.contains("beta"))
            .expect("beta row");
        let green_col = rows[green_row].find(STATUS_DOT).expect("beta dot") as u16;
        assert_eq!(
            buf.cell((green_col, green_row as u16)).unwrap().fg,
            probability_color(100, 100)
        );
    }

    // ----- snapshot matrix (widths and heights from task) -----

    fn snapshot_models() -> Vec<CachedModel> {
        // Four vendors so width-tier collapse is observable. Scores stagger
        // so phase ranks are deterministic.
        vec![
            vendor_model_with_axis_score(VendorKind::Kimi, "kimi-k2-thinking", 80.0, 0),
            vendor_model_with_axis_score(VendorKind::Claude, "claude-opus-4-5", 90.0, 0),
            vendor_model_with_axis_score(VendorKind::Codex, "gpt-5-codex", 70.0, 0),
            vendor_model_with_axis_score(VendorKind::Gemini, "gemini-2.5-pro", 60.0, 0),
        ]
    }

    #[test]
    fn snapshot_matrix_widths() {
        let models = snapshot_models();
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;

        for &width in &[200u16, 120, 100, 80, 60, 40, 30] {
            // Height is at the 50-row threshold so we exercise full-mode width
            // tiers across the matrix instead of the sub-50 compact override.
            let (lines, mode) = responsive_models_area(
                &models,
                &versions,
                &[],
                width,
                50,
                ModelsAreaMode::FullTable,
            );
            assert_eq!(mode, ModelsAreaMode::FullTable, "width {width}: mode");
            assert_eq!(
                lines.len() as u16,
                visible_count,
                "width {width}: line count must equal visible count"
            );

            for (i, line) in lines.iter().enumerate() {
                let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
                assert!(
                    total <= width as usize,
                    "width {width}: row {i} exceeds budget ({total})"
                );
            }
        }
    }

    #[test]
    fn snapshot_matrix_heights_drives_mode() {
        // Task: snapshot tests at heights 30, 20, 15, 12, 10 (terminal
        // heights, not pre-computed budgets). Below 50 rows the fixed compact
        // threshold wins over the normal budget hysteresis:
        //   term_h=30 → compact
        //   term_h=20 → compact
        //   term_h=15 → compact
        //   term_h=12 → compact
        //   term_h=10 → omit (preserves prev_mode)
        let models = snapshot_models();
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;
        assert_eq!(
            visible_count, 4,
            "fixture must keep four vendors visible for the matrix"
        );

        struct Case {
            term_h: u16,
            expect_mode: ModelsAreaMode,
            expect_line_count: usize,
        }
        let cases = [
            Case {
                term_h: 30,
                expect_mode: ModelsAreaMode::CompactQuota,
                expect_line_count: 1,
            },
            Case {
                term_h: 20,
                expect_mode: ModelsAreaMode::CompactQuota,
                expect_line_count: 1,
            },
            Case {
                term_h: 15,
                expect_mode: ModelsAreaMode::CompactQuota,
                expect_line_count: 1,
            },
            Case {
                term_h: 12,
                expect_mode: ModelsAreaMode::CompactQuota,
                expect_line_count: 1,
            },
            Case {
                term_h: 10,
                // Omitted area preserves prev_mode (compact, from term_h=12).
                expect_mode: ModelsAreaMode::CompactQuota,
                expect_line_count: 0,
            },
        ];

        let mut prev = ModelsAreaMode::FullTable;
        for case in cases {
            let (lines, mode) =
                responsive_models_area(&models, &versions, &[], 120, case.term_h, prev);
            assert_eq!(
                mode, case.expect_mode,
                "term_h={}: mode mismatch",
                case.term_h
            );
            assert_eq!(
                lines.len(),
                case.expect_line_count,
                "term_h={}: line count mismatch",
                case.term_h
            );
            prev = mode;
        }
    }

    #[test]
    fn snapshot_matrix_heights_omit_then_grow_back_requires_headroom() {
        // Same fixture as the height matrix above, but starting from
        // prev=FullTable: a transient drop into omit must not flip the
        // preserved mode, so when the terminal grows back to a full-table
        // budget we stay in full mode.
        let models = snapshot_models();
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;

        let mut prev = ModelsAreaMode::FullTable;

        // Drop into omit (term_h=10 → budget=0) while prev=full.
        let (lines, mode) = responsive_models_area(&models, &versions, &[], 120, 10, prev);
        assert!(lines.is_empty());
        assert_eq!(mode, ModelsAreaMode::FullTable, "omit preserves prev_mode");
        prev = mode;

        // Grow to exactly the boundary; full mode still requires headroom.
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(visible_count),
            prev,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        let (_, mode) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        assert_eq!(mode, ModelsAreaMode::FullTable);
    }

    #[test]
    fn snapshot_compact_at_width_60_keeps_full_vendor_labels() {
        let models = snapshot_models();
        let versions = build_version_index(&models);
        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &[],
            60,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );
        let row = full_buffer_line(&lines, 0, 60);
        // Width 60 is at the boundary: full vendor labels apply (the
        // 2-letter rule kicks in *below* 60).
        assert!(row.contains("kimi"), "kimi present at width 60: {row:?}");
    }

    // ----- regression tests for task 2: responsive score column -----

    #[test]
    fn width_tier_selection_at_50_picks_toprank_or_none() {
        // Acceptance criterion 1a: at width 50, TopRank or None shown (not Ipbr).
        let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 50);

        // Width 50 should not show full Ipbr (15 cols) — that requires more space.
        // TopRank (3 cols) or None should be chosen based on name budget.
        assert!(
            !row.contains("I ")
                || !row.contains("P ")
                || !row.contains("B ")
                || !row.contains("R "),
            "width 50 must not render full Ipbr tier: {row:?}"
        );
    }

    #[test]
    fn width_tier_selection_at_45_empirically_fits_toprank() {
        // Acceptance criterion 1a: at width 45, TopRank fits if budget >= NAME_WIDTH_MIN.
        let models = vec![model_with_axis_score("short", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 45, 50, ModelsAreaMode::FullTable);

        // With a short name, width 45 should empirically fit TopRank (3 cols).
        // name_budget_for(45, vendor_width=6, TopRank=3) = 45 - (6+1+1+1+4+1+1+3) = 27
        // 27 >= 8 (NAME_WIDTH_MIN), so TopRank should be chosen.
        assert!(
            !lines.is_empty(),
            "models should render at width 45 with short name"
        );
    }

    #[test]
    fn scores_right_anchored_row_spans_equal_width() {
        // Acceptance criterion 1b: at width 120, row total spans = 120 cols.
        let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);

        // Check that the row's total visible width equals 120.
        for (i, line) in lines.iter().enumerate() {
            let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(
                total, 120,
                "row {i} total span width must equal 120 (scores right-anchored)"
            );
        }
    }

    #[test]
    fn verbose_tier_renders_full_labels_with_three_space_separation() {
        // Acceptance criterion 1c: at width >= 63, IpbrVerbose tier chosen.
        let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 63, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 63);

        // IpbrVerbose should render full phase labels.
        assert!(
            row.contains("Idea"),
            "verbose tier: missing 'Idea': {row:?}"
        );
        assert!(
            row.contains("Plan"),
            "verbose tier: missing 'Plan': {row:?}"
        );
        assert!(
            row.contains("Build"),
            "verbose tier: missing 'Build': {row:?}"
        );
        assert!(
            row.contains("Review"),
            "verbose tier: missing 'Review': {row:?}"
        );

        // Check three-space separation between cells (e.g., "Idea 50   Plan 50").
        // The pattern should be: label + two-digit + three-spaces + label.
        let idea_to_plan = row.find("Idea").and_then(|i| {
            let tail = &row[i..];
            tail.find("   Plan").map(|offset| &tail[..offset + 7])
        });
        assert!(
            idea_to_plan.is_some(),
            "verbose tier: three spaces should separate Idea and Plan: {row:?}"
        );
    }

    #[test]
    fn term_h_below_50_forces_compact_quota() {
        let models = vec![
            model_with_axis_score("gpt-a", 1.0, 0),
            model_with_axis_score("gpt-b", 1.0, 1),
        ];
        let versions = build_version_index(&models);

        let (lines, mode) =
            responsive_models_area(&models, &versions, &[], 120, 49, ModelsAreaMode::FullTable);
        assert_eq!(
            mode,
            ModelsAreaMode::CompactQuota,
            "term_h=49 must force compact"
        );
        assert!(!lines.is_empty(), "compact quota should produce output");

        let (_, mode) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        assert_eq!(
            mode,
            ModelsAreaMode::FullTable,
            "term_h=50 follows normal hysteresis"
        );
    }
    #[test]
    fn full_table_orders_by_build_score_descending() {
        let m1 = vendor_model_with_axis_score(VendorKind::Codex, "gpt-alpha", 0.5, 0);
        let m2 = vendor_model_with_axis_score(VendorKind::Claude, "claude-beta", 0.75, 0);
        let m3 = vendor_model_with_axis_score(VendorKind::Gemini, "gemini-gamma", 1.0, 0);
        let models = vec![m1, m2, m3];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        let rows = render_to_text(&lines, 120);
        println!("ROWS 3: {:?}", rows);

        assert!(rows[0].contains("gemini"));
        assert!(rows[1].contains("claude"));
        assert!(rows[2].contains("codex"));
    }

    #[test]
    fn full_table_renders_vendor_label_on_every_row() {
        let m1 = vendor_model_with_axis_score(VendorKind::Claude, "claude-alpha", 100.0, 0);
        let m2 = vendor_model_with_axis_score(VendorKind::Claude, "claude-beta", 50.0, 0);
        let models = vec![m1, m2];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        let rows = render_to_text(&lines, 120);

        assert!(rows[0].contains("claude"));
        assert!(rows[1].contains("claude"));
    }

    #[test]
    fn compact_quota_renders_expanded_quota_when_space_permits() {
        let models = vec![vendor_model_with_axis_score(
            VendorKind::Claude,
            "claude-alpha",
            100.0,
            0,
        )];
        let versions = build_version_index(&models);

        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );
        let row = full_buffer_line(&lines, 0, 120);
        println!("ROW 2: {:?}", row);

        assert!(row.contains("Quota 100%"));
    }

    #[test]
    fn compact_quota_renders_narrow_quota_when_tight() {
        let models = vec![
            vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Codex, "gpt-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Gemini, "gemini-1", 50.0, 0),
        ];
        let versions = build_version_index(&models);

        let (lines, _) = responsive_models_area(
            &models,
            &versions,
            &[],
            50,
            term_h_for_budget(1),
            ModelsAreaMode::CompactQuota,
        );
        let row = full_buffer_line(&lines, 0, 50);
        println!("ROW: {:?}", row);

        assert!(!row.contains("Quota"));
        assert!(row.contains("100%"));
    }

    #[test]
    fn full_table_expands_quota_and_phase_labels_when_space_permits() {
        let models = vec![vendor_model_with_axis_score(
            VendorKind::Claude,
            "claude",
            100.0,
            0,
        )];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
        let row = full_buffer_line(&lines, 0, 120);

        assert!(row.contains("Quota"));
        assert!(row.contains("Idea"));
    }
}
