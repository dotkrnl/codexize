//! Responsive models area: full-table, compact-quota, or omitted, chosen
//! from the terminal height with hysteresis. Pure render given prev-mode.
//!
//! The renderer is the source of truth for the spec's mode-selection rule:
//! callers pass the terminal height and the renderer derives
//! `models_budget = term_h - 11` (1 each for top rule, bottom rule, keymap;
//! 8 for pipeline body floor — see spec §"Mode-selection rule"). Keeping the
//! `- 11` here means a single change site if the chrome reservation ever
//! shifts.
pub use super::state::ModelsAreaMode;
use super::state::{
    ProbColumn, QuotaColumn, RESET_TIME_MAX_WIDTH, ResetColumn, VERY_WIDE_THRESHOLD, choose_mode,
    format_name_with_freshness, name_budget_for, name_width_min, probability_color,
    probability_percent,
};
use crate::app::models::{subscription_color, subscription_tag};
use crate::model_names;
use crate::selection::{
    CachedModel, QuotaError, SubscriptionKind,
    config::SelectionPhase,
    display::{build_rank_order, phase_rank, visible_models},
    ranking::candidate_pool_weights,
};
use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::cmp::Ordering;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;
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
    quota_errors: &[QuotaError],
    width: u16,
    term_h: u16,
    prev_mode: ModelsAreaMode,
) -> (Vec<Line<'static>>, ModelsAreaMode) {
    let visible = visible_models(models);
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
    let full_row_count = visible_count.saturating_add(1);
    let mode = choose_mode(full_row_count, models_budget, prev_mode);
    let lines = match mode {
        ModelsAreaMode::FullTable => render_full_table(models, quota_errors, width),
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
    for &min_budget in &[max_req_name_width, name_width_min()] {
        if width >= VERY_WIDE_THRESHOLD {
            for &(quota, prob) in &layouts {
                if name_budget_for(width, vendor_width, quota, prob, ResetColumn::Shown)
                    >= min_budget
                {
                    return (quota, prob, ResetColumn::Shown);
                }
            }
        }
        for &(quota, prob) in &layouts {
            if name_budget_for(width, vendor_width, quota, prob, ResetColumn::Hidden) >= min_budget
            {
                return (quota, prob, ResetColumn::Hidden);
            }
        }
    }
    (QuotaColumn::Narrow, ProbColumn::None, ResetColumn::Hidden)
}
fn render_full_table(
    models: &[CachedModel],
    quota_errors: &[QuotaError],
    width: u16,
) -> Vec<Line<'static>> {
    let visible_set = visible_models(models);
    let max_req_name_width = models
        .iter()
        .filter(|m| visible_set.contains(&m.name))
        .map(|model| {
            let short_name =
                model_names::display_short(&model.name).expect("visible model must be curated");
            short_name.width()
        })
        .max()
        .unwrap_or(0);
    let vendor_width = vendor_column_width();
    let (quota_col, prob_col, reset_col) = choose_layout(width, vendor_width, max_req_name_width);
    let name_width =
        name_budget_for(width, vendor_width, quota_col, prob_col, reset_col).max(name_width_min());
    // Sampling probability cells are sourced from the candidate-pool
    // scorer in ranking.rs: softmax over phase rank weighted by relative
    // quota pressure, normalized within the assembled set. That keeps the
    // % column distinct from the rank column (bolding flags rank-1, the
    // number reports the row's sampling share). Models without an ipbr
    // phase score or with known zero quota receive weight 0.
    let model_refs: Vec<&CachedModel> = models.iter().collect();
    let idea_weights = candidate_pool_weights(&model_refs, SelectionPhase::Idea);
    let planning_weights = candidate_pool_weights(&model_refs, SelectionPhase::Planning);
    let build_weights = candidate_pool_weights(&model_refs, SelectionPhase::Build);
    let review_weights = candidate_pool_weights(&model_refs, SelectionPhase::Review);
    let weight_for = |phase: SelectionPhase, model: &CachedModel| -> f64 {
        let weights = match phase {
            SelectionPhase::Idea => &idea_weights,
            SelectionPhase::Planning => &planning_weights,
            SelectionPhase::Build => &build_weights,
            SelectionPhase::Review => &review_weights,
        };
        models
            .iter()
            .position(|candidate| std::ptr::eq(candidate, model))
            .map(|index| weights[index])
            .unwrap_or(0.0)
    };
    let total_for = |phase: SelectionPhase| -> f64 {
        match phase {
            SelectionPhase::Idea => idea_weights.iter().sum(),
            SelectionPhase::Planning => planning_weights.iter().sum(),
            SelectionPhase::Build => build_weights.iter().sum(),
            SelectionPhase::Review => review_weights.iter().sum(),
        }
    };
    let total_idea = total_for(SelectionPhase::Idea);
    let total_planning = total_for(SelectionPhase::Planning);
    let total_build = total_for(SelectionPhase::Build);
    let total_review = total_for(SelectionPhase::Review);
    let idea_ranks = phase_rank(models, SelectionPhase::Idea);
    let planning_ranks = phase_rank(models, SelectionPhase::Planning);
    let build_ranks = phase_rank(models, SelectionPhase::Build);
    let review_ranks = phase_rank(models, SelectionPhase::Review);
    let max_for = |totals: f64, phase: SelectionPhase| -> u8 {
        models
            .iter()
            .map(|m| probability_percent(weight_for(phase, m), totals))
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
    visible_models_list.sort_by(|a, b| {
        let a_weight = weight_for(SelectionPhase::Build, a);
        let b_weight = weight_for(SelectionPhase::Build, b);
        b_weight
            .partial_cmp(&a_weight)
            .unwrap_or(Ordering::Equal)
            .then_with(|| build_rank_order(a, b))
    });
    let mut lines: Vec<Line<'static>> = quota_summary_line(models, quota_errors, width)
        .into_iter()
        .collect();
    for model in visible_models_list {
        // Tag the row by curated model brand. Rows with no candidates stay
        // greyed out: arbitration already declines to pick them, so the
        // dim tag mirrors that "informational only" state.
        let selected_subscription = model.selected_candidate().map(|c| c.subscription);
        let label = display_vendor_tag(model);
        let color = match selected_subscription {
            Some(sub) => subscription_color(sub),
            None => Color::DarkGray,
        };
        let short_name =
            model_names::display_short(&model.name).expect("visible model must be curated");
        let vendor_failed = quota_errors
            .iter()
            .any(|err| err.subscription == model.subscription);
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
                QuotaColumn::Expanded => ("Quota  --%".to_string(), Color::Red),
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
                    QuotaColumn::Expanded => ("Quota  --%".to_string(), Color::DarkGray),
                    QuotaColumn::Narrow => (" --%".to_string(), Color::DarkGray),
                },
            }
        };
        let quota_span = Span::styled(quota_text, Style::default().fg(quota_color));
        let mut spans: Vec<Span<'static>> =
            vec![vendor_span, Span::raw(" "), dot_span, Span::raw(" ")];
        spans.extend(format_name_with_freshness(short_name, name_width));
        let phase_data = [
            (
                "Idea ",
                "I",
                SelectionPhase::Idea,
                total_idea,
                max_idea,
                idea_ranks.get(&model.name) == Some(&1),
            ),
            (
                "Plan ",
                "P",
                SelectionPhase::Planning,
                total_planning,
                max_planning,
                planning_ranks.get(&model.name) == Some(&1),
            ),
            (
                "Build ",
                "B",
                SelectionPhase::Build,
                total_build,
                max_build,
                build_ranks.get(&model.name) == Some(&1),
            ),
            (
                "Review ",
                "R",
                SelectionPhase::Review,
                total_review,
                max_review,
                review_ranks.get(&model.name) == Some(&1),
            ),
        ];
        match prob_col {
            ProbColumn::IpbrVerbose | ProbColumn::Ipbr => {
                let verbose = matches!(prob_col, ProbColumn::IpbrVerbose);
                let sep = if verbose { "   " } else { " " };
                for (idx, (long, short, phase, total, max, is_top)) in phase_data.iter().enumerate()
                {
                    spans.push(Span::raw(if idx == 0 { " " } else { sep }));
                    let label = if verbose { *long } else { *short };
                    let pct = probability_percent(weight_for(*phase, model), *total);
                    spans.push(if vendor_failed {
                        probability_unavailable_span(label)
                    } else {
                        probability_span(label, pct, *max, *is_top)
                    });
                }
            }
            ProbColumn::TopRank => {
                spans.push(Span::raw(" "));
                spans.push(top_rank_prob_span(
                    vendor_failed,
                    &phase_data.map(|(_, short, phase, total, max, is_top)| {
                        (
                            is_top,
                            short,
                            probability_percent(weight_for(phase, model), total),
                            max,
                        )
                    }),
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
        spans.push(Span::raw(" "));
        spans.push(quota_span);
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
/// Bracketed brand tag drawn in the vendor column. The tag text comes only
/// from the model's curated `display_vendor` (e.g. `[deepseek]`, `[claude]`).
fn display_vendor_tag(model: &CachedModel) -> String {
    let dv =
        crate::model_names::display_vendor(&model.name).expect("visible model must be curated");
    format!("[{dv}]")
}
/// Width of the vendor-tag column (padded). Sized for the widest curated
/// brand-tag, `[deepseek]` / `[opencode]` (10 chars).
fn vendor_column_width() -> usize {
    10
}
fn top_rank_prob_span(vendor_failed: bool, candidates: &[(bool, &str, u8, u8)]) -> Span<'static> {
    if vendor_failed {
        return probability_unavailable_span("P");
    }
    if let Some((_, label, pct, max)) = candidates.iter().find(|c| c.0) {
        return probability_span(label, *pct, *max, true);
    }
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
fn render_compact_quota(
    models: &[CachedModel],
    quota_errors: &[QuotaError],
    width: u16,
) -> Vec<Line<'static>> {
    quota_summary_line(models, quota_errors, width)
        .into_iter()
        .collect()
}

#[derive(Clone, Copy)]
struct QuotaSummaryEntry<'a> {
    subscription: SubscriptionKind,
    model: &'a CachedModel,
    failed: bool,
}

#[derive(Clone, Copy)]
enum QuotaSummaryForm {
    Long,
    Mid,
    Short,
    Extreme,
}

fn quota_summary_line(
    models: &[CachedModel],
    quota_errors: &[QuotaError],
    width: u16,
) -> Option<Line<'static>> {
    if width == 0 {
        return None;
    }
    let entries = quota_summary_entries(models, quota_errors);
    if entries.is_empty() {
        return None;
    }
    for form in [
        QuotaSummaryForm::Long,
        QuotaSummaryForm::Mid,
        QuotaSummaryForm::Short,
        QuotaSummaryForm::Extreme,
    ] {
        let line = build_quota_summary_line(&entries, form, width as usize);
        if line_width(&line) <= width as usize {
            return Some(line);
        }
    }
    None
}

fn quota_summary_entries<'a>(
    models: &'a [CachedModel],
    quota_errors: &[QuotaError],
) -> Vec<QuotaSummaryEntry<'a>> {
    let order = [
        SubscriptionKind::Claude,
        SubscriptionKind::Codex,
        SubscriptionKind::Gemini,
        SubscriptionKind::Kimi,
        SubscriptionKind::OpencodeGo,
        SubscriptionKind::Direct,
    ];
    let mut entries = Vec::new();
    for subscription in order {
        if let Some(model) = models
            .iter()
            .filter(|m| m.subscription == subscription)
            .min_by(|a, b| build_rank_order(a, b))
        {
            let failed = quota_errors
                .iter()
                .any(|err| err.subscription == subscription);
            entries.push(QuotaSummaryEntry {
                subscription,
                model,
                failed,
            });
        }
    }
    entries
}

fn build_quota_summary_line(
    entries: &[QuotaSummaryEntry<'_>],
    form: QuotaSummaryForm,
    target_width: usize,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    match form {
        QuotaSummaryForm::Long => {
            spans.push(Span::styled(
                "Remaining Quota: ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        QuotaSummaryForm::Mid => {
            spans.push(Span::styled(
                "Quota: ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        QuotaSummaryForm::Short | QuotaSummaryForm::Extreme => {}
    }
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            let sep = match form {
                QuotaSummaryForm::Long | QuotaSummaryForm::Mid => ", ",
                QuotaSummaryForm::Short | QuotaSummaryForm::Extreme => " ",
            };
            spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        }
        let label = match form {
            QuotaSummaryForm::Long | QuotaSummaryForm::Mid | QuotaSummaryForm::Short => {
                subscription_tag(entry.subscription).to_string()
            }
            QuotaSummaryForm::Extreme => {
                quota_summary_extreme_label(entry.subscription).to_string()
            }
        };
        spans.push(Span::styled(
            label,
            Style::default().fg(subscription_color(entry.subscription)),
        ));
        let separator = match form {
            QuotaSummaryForm::Long | QuotaSummaryForm::Mid | QuotaSummaryForm::Short => " ",
            QuotaSummaryForm::Extreme => "",
        };
        spans.push(Span::raw(separator));
        push_quota_value(&mut spans, *entry, form);
        if matches!(form, QuotaSummaryForm::Long)
            && let Some(quota_resets_at) = entry.model.quota_resets_at
        {
            spans.push(Span::styled(" (", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                format_reset_time(quota_resets_at),
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(")", Style::default().fg(Color::DarkGray)));
        }
    }
    let used = spans.iter().map(|span| span.content.width()).sum::<usize>();
    if used < target_width {
        spans.push(Span::raw(" ".repeat(target_width - used)));
    }
    Line::from(spans)
}

fn push_quota_value(
    spans: &mut Vec<Span<'static>>,
    entry: QuotaSummaryEntry<'_>,
    form: QuotaSummaryForm,
) {
    if entry.failed {
        spans.push(Span::styled(
            quota_unknown_text(form).to_string(),
            Style::default().fg(Color::Red),
        ));
        return;
    }
    match entry.model.quota_percent {
        Some(value) => spans.push(Span::styled(
            quota_value_text(value, form),
            Style::default().fg(probability_color(value, 100)),
        )),
        None => spans.push(Span::styled(
            quota_unknown_text(form).to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    }
}

fn quota_value_text(value: u8, form: QuotaSummaryForm) -> String {
    match form {
        QuotaSummaryForm::Long | QuotaSummaryForm::Mid => format!("{value}%"),
        QuotaSummaryForm::Short | QuotaSummaryForm::Extreme => value.to_string(),
    }
}

fn quota_unknown_text(form: QuotaSummaryForm) -> &'static str {
    match form {
        QuotaSummaryForm::Long | QuotaSummaryForm::Mid => "--%",
        QuotaSummaryForm::Short | QuotaSummaryForm::Extreme => "--",
    }
}

fn quota_summary_extreme_label(subscription: SubscriptionKind) -> &'static str {
    match subscription {
        SubscriptionKind::Claude => "cl",
        SubscriptionKind::Codex => "co",
        SubscriptionKind::Gemini => "ge",
        SubscriptionKind::Kimi => "ki",
        SubscriptionKind::OpencodeGo => "op",
        SubscriptionKind::Direct => "di",
    }
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}
#[cfg(test)]
#[path = "tests_mod.rs"]
mod tests_mod;
