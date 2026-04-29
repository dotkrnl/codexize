use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Clear, Paragraph},
};

pub fn render_modal_overlay(
    frame: &mut Frame,
    terminal_area: Rect,
    title: Option<&str>,
    border_style: Style,
    content: Vec<Line<'static>>,
    keymap_line: Line<'static>,
) {
    // 3. Dim the background behind the modal.
    let dim = Paragraph::new("").style(Style::default().bg(Color::DarkGray));
    frame.render_widget(dim, terminal_area);

    let terminal_width = terminal_area.width;
    let terminal_height = terminal_area.height;
    let max_w = terminal_width.saturating_sub(4).max(1);
    let dialog_w = max_w.min(80).max(max_w.min(40));
    let content_h = content.len();
    // 6. Increase internal padding (+5 instead of +3) and reserve more vertical margin.
    let dialog_h = ((content_h + 5) as u16).min(terminal_height.saturating_sub(4));
    let dialog = Rect::new(
        (terminal_width.saturating_sub(dialog_w)) / 2,
        (terminal_height.saturating_sub(dialog_h)) / 2,
        dialog_w,
        dialog_h,
    );

    // 5. Drop shadow — render a dark block offset by (1, 1).
    let shadow_rect = Rect::new(
        (dialog.x + 1).min(terminal_area.x + terminal_area.width),
        (dialog.y + 1).min(terminal_area.y + terminal_area.height),
        dialog_w.saturating_sub(1),
        dialog_h.saturating_sub(1),
    );
    let shadow = Paragraph::new("").style(Style::default().bg(Color::Black));
    frame.render_widget(shadow, shadow_rect);

    frame.render_widget(Clear, dialog);

    // 1. Solid background, 2B. Semantic border colour, 4. Optional title.
    let mut block = Block::bordered()
        .border_style(border_style)
        .style(Style::default().bg(Color::Black));
    if let Some(t) = title {
        block = block.title(t.to_string());
    }
    frame.render_widget(block.clone(), dialog);
    let inner = block.inner(dialog);

    if inner.height > 0 && inner.width > 0 {
        let inner_h = inner.height as usize;
        let content_capacity = inner_h.saturating_sub(1);
        let lines_to_write: Vec<Line<'static>> = if content.len() <= content_capacity {
            content
        } else {
            let keep = content_capacity.saturating_sub(1);
            let mut truncated: Vec<Line<'static>> = content.into_iter().take(keep).collect();
            truncated.push(Line::from("…"));
            truncated
        };

        let buf = frame.buffer_mut();
        for (offset, line) in lines_to_write.iter().enumerate() {
            buf.set_line(inner.x, inner.y + offset as u16, line, inner.width);
        }
        buf.set_line(
            inner.x,
            inner.y + inner.height - 1,
            &keymap_line,
            inner.width,
        );
    }
}
