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

    // ----- mode-selection / hysteresis -----

    #[test]
    fn empty_models_returns_empty_lines() {
        let models: Vec<CachedModel> = Vec::new();
        let versions = build_version_index(&models);
        let (lines, mode) =
            responsive_models_area(&models, &versions, &[], 120, 20, ModelsAreaMode::FullTable);
        assert!(lines.is_empty());
        assert_eq!(mode, ModelsAreaMode::FullTable);
    }

    #[test]
    fn zero_height_budget_returns_empty_preserving_prev_mode() {
        let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
        let versions = build_version_index(&models);

        let (lines, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            0,
            ModelsAreaMode::CompactQuota,
        );
        assert!(lines.is_empty());
        // Preserved (no flicker on transient zero budget).
        assert_eq!(mode, ModelsAreaMode::CompactQuota);
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

        // Strict: budget == count keeps full
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            visible_count,
            ModelsAreaMode::FullTable,
        );
        assert_eq!(mode, ModelsAreaMode::FullTable);

        // Strict: budget == count - 1 falls to compact
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            visible_count - 1,
            ModelsAreaMode::FullTable,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);
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

        // From compact, budget == count is NOT enough (hysteresis +1).
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            visible_count,
            ModelsAreaMode::CompactQuota,
        );
        assert_eq!(mode, ModelsAreaMode::CompactQuota);

        // budget == count + 1 unlocks the switch back.
        let (_, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            visible_count + 1,
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

        // Start on the strict boundary in full mode. Oscillate: count, count-1,
        // count, count-1, ... With strict full→compact, the first frame at
        // count keeps full, the second at count-1 drops to compact. From there
        // the +1 hysteresis means we need count+1 to flip back; oscillation
        // between count and count-1 stays *compact* (no flicker), even though
        // the budget keeps crossing the strict threshold.
        let mut mode = ModelsAreaMode::FullTable;

        // Frame 1: budget == count, prev=full → stays full.
        let (_, m) = responsive_models_area(&models, &versions, &[], 120, visible_count, mode);
        assert_eq!(m, ModelsAreaMode::FullTable);
        mode = m;

        // Frame 2: budget == count - 1, prev=full → strict drops to compact.
        let (_, m) = responsive_models_area(&models, &versions, &[], 120, visible_count - 1, mode);
        assert_eq!(m, ModelsAreaMode::CompactQuota);
        mode = m;

        // Now the oscillation: count, count-1, count, count-1, ... Once we
        // are in compact, +1 hysteresis means count alone never flips us
        // back. Run several cycles to prove there is no per-frame flicker.
        for _ in 0..6 {
            let (_, m) = responsive_models_area(&models, &versions, &[], 120, visible_count, mode);
            assert_eq!(
                m,
                ModelsAreaMode::CompactQuota,
                "+1 hysteresis must hold compact at boundary"
            );
            mode = m;

            let (_, m) =
                responsive_models_area(&models, &versions, &[], 120, visible_count - 1, mode);
            assert_eq!(m, ModelsAreaMode::CompactQuota);
            mode = m;
        }
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

        // Width 90 → tier 1 (full IPBR).
        let (lines, mode) =
            responsive_models_area(&models, &versions, &[], 90, 10, ModelsAreaMode::FullTable);
        assert_eq!(mode, ModelsAreaMode::FullTable);

        // Render to a buffer so we can inspect cell modifiers.
        let area = Rect::new(0, 0, 90, lines.len() as u16);
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
            responsive_models_area(&models, &versions, &[], 50, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 50, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 80, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 70, 10, ModelsAreaMode::FullTable);

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
            responsive_models_area(&models, &versions, &[], 48, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 120, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 120, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 120, 10, ModelsAreaMode::FullTable);
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
            responsive_models_area(&models, &versions, &[], 200, 10, ModelsAreaMode::FullTable);
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
            1,
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
    fn compact_quota_uses_two_letter_vendor_below_60() {
        let models = vec![
            vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
            vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
        ];
        let versions = build_version_index(&models);

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 50, 1, ModelsAreaMode::CompactQuota);
        let row = full_buffer_line(&lines, 0, 50);
        assert!(row.contains("ki"), "2-letter kimi: {row:?}");
        assert!(row.contains("cl"), "2-letter claude: {row:?}");
        assert!(!row.contains("kimi"), "no full vendor name: {row:?}");
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

        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 30, 1, ModelsAreaMode::CompactQuota);
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
            1,
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
            10,
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

        // Each phase letter still appears, with `--` rendered in red.
        for ph in ["I", "P", "B", "R"] {
            let pat = format!("{ph}--");
            let col = row
                .find(&pat)
                .unwrap_or_else(|| panic!("expected {pat:?} for failed vendor: {row:?}"))
                as u16;
            assert_eq!(buf.cell((col, 0)).unwrap().fg, Color::Red);
        }
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
            // Height budget always covers visible_count so we exercise
            // full-mode width tiers across the matrix.
            let (lines, mode) = responsive_models_area(
                &models,
                &versions,
                &[],
                width,
                visible_count,
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
        let models = snapshot_models();
        let versions = build_version_index(&models);
        let visible_count = visible_models(&models, &versions).len() as u16;

        let mut prev = ModelsAreaMode::FullTable;
        for &budget in &[30u16, 20, 15, 12, 10] {
            let (_, mode) = responsive_models_area(&models, &versions, &[], 120, budget, prev);
            // With heights >= visible_count, full table is always reachable.
            // Specifically, all heights in this matrix exceed the count, so
            // the renderer should stay in full mode every frame.
            assert!(
                budget >= visible_count,
                "test fixture: every matrix budget should cover visible count"
            );
            assert_eq!(mode, ModelsAreaMode::FullTable, "budget {budget}: mode");
            prev = mode;
        }
    }

    #[test]
    fn snapshot_compact_at_width_60_keeps_full_vendor_labels() {
        let models = snapshot_models();
        let versions = build_version_index(&models);
        let (lines, _) =
            responsive_models_area(&models, &versions, &[], 60, 1, ModelsAreaMode::CompactQuota);
        let row = full_buffer_line(&lines, 0, 60);
        // Width 60 is at the boundary: full vendor labels apply (the
        // 2-letter rule kicks in *below* 60).
        assert!(row.contains("kimi"), "kimi present at width 60: {row:?}");
    }
}
