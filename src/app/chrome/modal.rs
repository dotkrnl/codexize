use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

/// Paint the dim full-screen backdrop that recedes underlying TUI behind a
/// modal. Compose this around [`render_modal_overlay`] only when there is
/// underlying UI to recede; surfaces with no underlying TUI (e.g. preflight)
/// must skip the backdrop or it would flatten to a uniform gray screen.
pub fn render_modal_backdrop(frame: &mut Frame, area: Rect) {
    let dim = Paragraph::new("").style(Style::default().bg(Color::DarkGray));
    frame.render_widget(dim, area);
}

/// Shared modal width calculation so callers that pre-wrap body text can stay
/// in lockstep with [`render_modal_overlay`].
pub fn modal_inner_width(terminal_area: Rect) -> u16 {
    let max_w = terminal_area.width.saturating_sub(4).max(1);
    let dialog_w = max_w.min(80).max(max_w.min(40));
    dialog_w.saturating_sub(2)
}

/// Render the centered modal panel: dark surface, bold accent border, accent
/// bold title, body text in readable light gray, one blank separator row,
/// and a keymap row that is layout-reserved (body yields rows when height is
/// tight; the keymap stays).
///
/// The dim backdrop is composed *outside* this helper so callers without
/// underlying UI can opt out — see [`render_modal_backdrop`].
pub fn render_modal_overlay(
    frame: &mut Frame,
    terminal_area: Rect,
    accent: Color,
    title: Option<&str>,
    content: Vec<Line<'static>>,
    keymap_line: Line<'static>,
) {
    let terminal_width = terminal_area.width;
    let terminal_height = terminal_area.height;
    let dialog_w = modal_inner_width(terminal_area).saturating_add(2);
    let content_h = content.len();
    let dialog_h = ((content_h + 5) as u16).min(terminal_height.saturating_sub(4));
    let dialog = Rect::new(
        (terminal_width.saturating_sub(dialog_w)) / 2,
        (terminal_height.saturating_sub(dialog_h)) / 2,
        dialog_w,
        dialog_h,
    );

    frame.render_widget(Clear, dialog);

    let accent_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);

    let mut block = Block::bordered()
        .border_style(accent_style)
        .style(Style::default().bg(Color::Black));
    if let Some(t) = title {
        block = block.title(Span::styled(t.to_string(), accent_style));
    }
    frame.render_widget(block.clone(), dialog);
    let inner = block.inner(dialog);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let inner_h = inner.height as usize;
    // Reserve the last inner row for the keymap and the row above it as the
    // mandated blank separator. Body wraps/truncates into the rows above.
    let keymap_offset = inner_h.saturating_sub(1);
    let body_capacity = inner_h.saturating_sub(2);

    let lines_to_write: Vec<Line<'static>> = if body_capacity == 0 {
        Vec::new()
    } else if content.len() <= body_capacity {
        content.into_iter().map(dialog_body_line).collect()
    } else {
        let keep = body_capacity.saturating_sub(1);
        let mut truncated: Vec<Line<'static>> = content
            .into_iter()
            .take(keep)
            .map(dialog_body_line)
            .collect();
        truncated.push(dialog_body_line(Line::from("…")));
        truncated
    };
    let keymap_line = dialog_body_line(keymap_line);

    let buf = frame.buffer_mut();
    for (offset, line) in lines_to_write.iter().enumerate() {
        buf.set_line(inner.x, inner.y + offset as u16, line, inner.width);
    }
    if inner_h >= 1 {
        buf.set_line(
            inner.x,
            inner.y + keymap_offset as u16,
            &keymap_line,
            inner.width,
        );
    }
}

fn dialog_body_line(mut line: Line<'static>) -> Line<'static> {
    for span in &mut line.spans {
        span.style = span.style.bg(Color::Black);
        if matches!(span.style.fg, None | Some(Color::Black | Color::DarkGray)) {
            span.style = span.style.fg(Color::Gray);
        }
    }
    line
}
