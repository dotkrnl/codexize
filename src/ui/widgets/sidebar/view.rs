use crate::app_shell::{ShellFocus, SidebarRow, SidebarView};
use crate::state::Phase;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Fixed width for the sessions sidebar panel.
const SIDEBAR_WIDTH: u16 = 32;

pub fn sidebar_width() -> u16 {
    SIDEBAR_WIDTH
}

/// Render the sidebar into `area` of `buf`.
pub fn render_sidebar(area: Rect, buf: &mut Buffer, view: &SidebarView) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width;
    let mut y = area.y;

    // Header
    let header_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let header_text = " Sessions";
    let header_line = Line::from(Span::styled(header_text, header_style));
    buf.set_line(area.x, y, &header_line, width);
    y += 1;

    // Divider
    let divider = "─".repeat(width as usize);
    let divider_line = Line::from(Span::styled(divider, Style::default().fg(Color::DarkGray)));
    buf.set_line(area.x, y, &divider_line, width);
    y += 1;

    // Rows
    let available_height = area.height.saturating_sub(y - area.y + 1).max(1);
    let row_count = view.rows.len();
    if row_count == 0 {
        let empty = Line::from(Span::styled(
            " (no sessions)",
            Style::default().fg(Color::DarkGray),
        ));
        buf.set_line(area.x, y, &empty, width);
    } else {
        let start = view
            .selected_index
            .saturating_sub(available_height as usize - 1);
        let end = (start + available_height as usize).min(row_count);
        for (idx, row) in view.rows[start..end].iter().enumerate() {
            let actual_idx = start + idx;
            let is_selected = actual_idx == view.selected_index;
            let line = row_line(row, is_selected, view.focus == ShellFocus::Sidebar, width);
            buf.set_line(area.x, y, &line, width);
            y += 1;
        }
    }

    // Footer hint (always at bottom)
    if area.height > 2 {
        let hint_y = area.y + area.height - 1;
        let hint = if view.focus == ShellFocus::Sidebar {
            "↑↓ move  Enter open  Esc hide"
        } else {
            ""
        };
        let hint_line = Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)));
        buf.set_line(area.x, hint_y, &hint_line, width);
    }
}

fn row_line(
    row: &SidebarRow,
    is_selected: bool,
    sidebar_has_focus: bool,
    width: u16,
) -> Line<'static> {
    let mut spans = Vec::new();

    // Selection indicator
    let sel = if is_selected && sidebar_has_focus {
        ">"
    } else {
        " "
    };
    spans.push(Span::styled(sel, Style::default().fg(Color::Yellow)));

    // State indicators
    let focused_indicator = if row.focused {
        '●'
    } else if row.open {
        '○'
    } else {
        ' '
    };
    let running_indicator = if row.running { '▶' } else { ' ' };
    let indicators = format!("{}{}", focused_indicator, running_indicator);
    spans.push(Span::styled(indicators, Style::default().fg(Color::Cyan)));

    // Date + title (truncate to fit)
    let label = format!("{} {}", row.date_label, row.title.trim());
    let label_budget = width.saturating_sub(5).max(4) as usize;
    let label_text = if label.chars().count() > label_budget {
        let truncated: String = label.chars().take(label_budget.saturating_sub(1)).collect();
        format!("{}…", truncated)
    } else {
        label
    };
    spans.push(Span::raw(" "));
    spans.push(Span::styled(label_text, row_style(row)));

    let mut line = Line::from(spans);
    if is_selected {
        let bg = if sidebar_has_focus {
            Color::DarkGray
        } else {
            Color::Black
        };
        for span in &mut line.spans {
            span.style = span.style.bg(bg);
        }
    }
    line
}

fn row_style(row: &SidebarRow) -> Style {
    let fg = match row.phase {
        Phase::Done => Color::Green,
        Phase::BlockedNeedsUser => Color::Red,
        Phase::WaitingToImplement => Color::Yellow,
        Phase::Cancelled => Color::DarkGray,
        Phase::IdeaInput | Phase::SpecReviewPaused | Phase::PlanReviewPaused => Color::Blue,
        _ => Color::White,
    };
    let mut style = Style::default().fg(fg);
    if row.running {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}
