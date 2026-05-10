use ratatui::style::{Color, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelsAreaMode {
    #[default]
    FullTable,
    CompactQuota,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuotaColumn {
    Expanded,
    Narrow,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbColumn {
    IpbrVerbose,
    Ipbr,
    TopRank,
    None,
}
const NAME_WIDTH_MIN: usize = 8;
const ELLIPSIS: &str = "...";
pub(super) const RESET_TIME_MAX_WIDTH: usize = 12;
pub(super) fn choose_mode(
    visible_count: u16,
    models_budget: u16,
    prev_mode: ModelsAreaMode,
) -> ModelsAreaMode {
    match prev_mode {
        ModelsAreaMode::FullTable => {
            if models_budget > visible_count {
                ModelsAreaMode::FullTable
            } else {
                ModelsAreaMode::CompactQuota
            }
        }
        ModelsAreaMode::CompactQuota => {
            if models_budget >= visible_count.saturating_add(1) {
                ModelsAreaMode::FullTable
            } else {
                ModelsAreaMode::CompactQuota
            }
        }
    }
}
pub(super) fn name_budget_for(
    width: u16,
    vendor_width: usize,
    quota: QuotaColumn,
    prob_col: ProbColumn,
) -> usize {
    let fixed = full_row_fixed_width(vendor_width, quota, prob_col);
    (width as usize).saturating_sub(fixed)
}
pub(super) fn probability_percent(weight: f64, total: f64) -> u8 {
    if total <= 0.0 || weight <= 0.0 {
        return 0;
    }
    (weight / total * 100.0).round().clamp(0.0, 99.0) as u8
}
pub(super) fn probability_color(pct: u8, max_pct: u8) -> Color {
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
pub(super) fn format_name_with_freshness(short_name: &str, budget: usize) -> Vec<Span<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    let name_len = short_name.width();
    if name_len <= budget {
        let pad = budget - name_len;
        let mut spans = vec![Span::styled(short_name.to_string(), name_style())];
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        return spans;
    }
    let ellipsis_len = ELLIPSIS.width();
    if budget > ellipsis_len {
        let visible_width = budget - ellipsis_len;
        let mut truncated = String::new();
        let mut w = 0;
        for c in short_name.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > visible_width {
                break;
            }
            w += cw;
            truncated.push(c);
        }
        return vec![
            Span::styled(truncated, name_style()),
            Span::styled(ELLIPSIS, ellipsis_style()),
        ];
    }
    let mut truncated = String::new();
    let mut w = 0;
    for c in ELLIPSIS.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > budget {
            break;
        }
        w += cw;
        truncated.push(c);
    }
    vec![Span::styled(truncated, ellipsis_style())]
}
pub(super) fn name_width_min() -> usize {
    NAME_WIDTH_MIN
}
pub(super) fn full_row_fixed_width(
    vendor_width: usize,
    quota: QuotaColumn,
    prob_col: ProbColumn,
) -> usize {
    let probs = match prob_col {
        ProbColumn::IpbrVerbose => 40,
        ProbColumn::Ipbr => 15,
        ProbColumn::TopRank => 3,
        ProbColumn::None => 0,
    };
    let prob_separator = if probs == 0 { 0 } else { 1 };
    let quota_width = match quota {
        QuotaColumn::Expanded => 9,
        QuotaColumn::Narrow => 4,
    };
    vendor_width + 1 + 1 + 1 + quota_width + 1 + prob_separator + probs
}
fn name_style() -> Style {
    Style::default().fg(Color::Cyan)
}
fn ellipsis_style() -> Style {
    Style::default().fg(Color::DarkGray)
}
#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
