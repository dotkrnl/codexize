//! Responsive models area: full-table or compact-quota line, chosen by
//! `height_budget` with hysteresis. Pure render given prev-mode.

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

/// Pure renderer.
///
/// Returns the rendered lines (possibly empty when the budget is 0 or no
/// models are visible) plus the chosen mode. The mode must be persisted by
/// the caller and passed back as `prev_mode` next frame to honor the
/// hysteresis described in the spec.
pub fn responsive_models_area(
    models: &[CachedModel],
    versions: &VersionIndex,
    quota_errors: &[QuotaError],
    width: u16,
    height_budget: u16,
    prev_mode: ModelsAreaMode,
) -> (Vec<Line<'static>>, ModelsAreaMode) {
    let visible = visible_models(models, versions);
    let visible_count = visible.len() as u16;

    if visible_count == 0 || height_budget == 0 || width == 0 {
        // Nothing to draw. Preserve prev_mode so a transient zero budget
        // does not reset the hysteresis state.
        return (Vec::new(), prev_mode);
    }

    let mode = choose_mode(visible_count, height_budget, prev_mode);

    let lines = match mode {
        ModelsAreaMode::FullTable => render_full_table(models, versions, quota_errors, width),
        ModelsAreaMode::CompactQuota => render_compact_quota(models, quota_errors, width),
    };

    (lines, mode)
}

fn choose_mode(
    visible_count: u16,
    height_budget: u16,
    prev_mode: ModelsAreaMode,
) -> ModelsAreaMode {
    match prev_mode {
        ModelsAreaMode::FullTable => {
            // Full → Compact uses the strict threshold.
            if height_budget >= visible_count {
                ModelsAreaMode::FullTable
            } else {
                ModelsAreaMode::CompactQuota
            }
        }
        ModelsAreaMode::CompactQuota => {
            // Compact → Full requires +1 to absorb single-row resize jitter.
            if height_budget >= visible_count.saturating_add(1) {
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
enum ProbColumn {
    Ipbr,    // full "Ixx Pxx Bxx Rxx"
    TopRank, // single 3-col cell, top-rank phase
    None,
}

fn full_table_prob_column(width: u16) -> ProbColumn {
    if width >= 80 {
        ProbColumn::Ipbr
    } else if width >= 60 {
        ProbColumn::TopRank
    } else {
        ProbColumn::None
    }
}

fn vendor_abbreviated(width: u16) -> bool {
    width < 60
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

const NAME_WIDTH_DEFAULT: usize = 28;
const NAME_WIDTH_MIN: usize = 8;

fn vendor_label(vendor: VendorKind, abbreviated: bool) -> &'static str {
    match (vendor, abbreviated) {
        (VendorKind::Claude, false) => "claude",
        (VendorKind::Codex, false) => "codex",
        (VendorKind::Gemini, false) => "gemini",
        (VendorKind::Kimi, false) => "kimi",
        (VendorKind::Claude, true) => "cl",
        (VendorKind::Codex, true) => "cd",
        (VendorKind::Gemini, true) => "ge",
        (VendorKind::Kimi, true) => "ki",
    }
}

/// Width of the vendor-tag column (padded). 6 cols at default, 2 cols when
/// abbreviated.
fn vendor_column_width(abbreviated: bool) -> usize {
    if abbreviated { 2 } else { 6 }
}

/// Width consumed by everything except the model-name column.
///
/// Layout: `{vendor:vw} {dot} {quota:>4} {name + freshness} {probs}`
///
/// Counting separators (one space between each visible column):
///   vw + 1 + 1 + 1 + 4 + 1 + probs_width + (1 if probs)
fn full_row_fixed_width(vendor_width: usize, prob_col: ProbColumn) -> usize {
    let probs = match prob_col {
        ProbColumn::Ipbr => 15,
        ProbColumn::TopRank => 3,
        ProbColumn::None => 0,
    };
    let prob_separator = if probs == 0 { 0 } else { 1 };
    // vendor + sp + dot + sp + quota + sp + name (variable) + prob_sep + probs
    vendor_width + 1 + 1 + 1 + 4 + 1 + prob_separator + probs
}

fn name_budget(width: u16, vendor_width: usize, prob_col: ProbColumn) -> usize {
    let fixed = full_row_fixed_width(vendor_width, prob_col);
    let raw = (width as usize).saturating_sub(fixed);
    raw.clamp(NAME_WIDTH_MIN, NAME_WIDTH_DEFAULT)
}

fn render_full_table(
    models: &[CachedModel],
    versions: &VersionIndex,
    quota_errors: &[QuotaError],
    width: u16,
) -> Vec<Line<'static>> {
    let prob_col = full_table_prob_column(width);
    let abbreviated = vendor_abbreviated(width);
    let vendor_width = vendor_column_width(abbreviated);
    let name_width = name_budget(width, vendor_width, prob_col);

    let visible_set = visible_models(models, versions);

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

    let mut vendor_order: Vec<VendorKind> = Vec::new();
    let mut by_vendor: std::collections::BTreeMap<VendorKind, Vec<&CachedModel>> =
        std::collections::BTreeMap::new();
    for model in models.iter().filter(|m| visible_set.contains(&m.name)) {
        if !vendor_order.contains(&model.vendor) {
            vendor_order.push(model.vendor);
        }
        by_vendor.entry(model.vendor).or_default().push(model);
    }
    for group in by_vendor.values_mut() {
        group.sort_by_key(|m| m.display_order);
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for vendor in &vendor_order {
        let label = vendor_label(*vendor, abbreviated);
        let color = vendor_color(*vendor);
        let prefix = vendor_prefix(*vendor);
        let group = &by_vendor[vendor];

        for (i, model) in group.iter().enumerate() {
            let short_name = model_names::display_name_for_vendor(&model.name, prefix);

            let vendor_failed = quota_errors.iter().any(|err| err.vendor == model.vendor);

            // First row in a vendor group prints the label; subsequent rows
            // pad with blanks to keep the column visually aligned.
            let vendor_span = if i == 0 {
                Span::styled(
                    format!("{label:<vendor_width$}"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(" ".repeat(vendor_width))
            };

            let dot_color = if vendor_failed {
                Color::Red
            } else {
                let score = model.current_score.round().clamp(0.0, 100.0) as u8;
                probability_color(score, 100)
            };
            let dot_span = Span::styled(STATUS_DOT, Style::default().fg(dot_color));

            let (quota_text, quota_color) = if vendor_failed {
                (" --%".to_string(), Color::Red)
            } else {
                match model.quota_percent {
                    Some(v) => (format!("{v:>3}%"), probability_color(v, 100)),
                    None => (" --%".to_string(), Color::DarkGray),
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
                ProbColumn::Ipbr => {
                    let idea_pct =
                        probability_percent(prob_for(SelectionPhase::Idea, model), total_idea);
                    let planning_pct = probability_percent(
                        prob_for(SelectionPhase::Planning, model),
                        total_planning,
                    );
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

    let abbreviated = vendor_abbreviated(width);

    // One entry per vendor, ordered Claude · Codex · Gemini · Kimi (fixed
    // identity ordering — the spec sample uses this order).
    // We render only vendors that appear in the model set so the line tracks
    // the configured selection. For each vendor, pick the model with the
    // best `current_score` to source the quota — this matches the
    // per-vendor "headline" semantics of the compact line.
    let order = [
        VendorKind::Kimi,
        VendorKind::Claude,
        VendorKind::Codex,
        VendorKind::Gemini,
    ];

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut first = true;
    for vendor in order {
        let Some(model) = models.iter().filter(|m| m.vendor == vendor).max_by(|a, b| {
            a.current_score
                .partial_cmp(&b.current_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            continue;
        };

        let vendor_failed = quota_errors.iter().any(|err| err.vendor == vendor);

        if !first {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        first = false;

        let label = vendor_label(vendor, abbreviated);
        spans.push(Span::styled(
            label.to_string(),
            Style::default().fg(vendor_color(vendor)),
        ));
        spans.push(Span::raw(" "));

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

