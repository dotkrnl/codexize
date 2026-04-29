use ratatui::{
    Frame,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Clear},
};

pub fn render_modal_overlay(
    frame: &mut Frame,
    terminal_area: ratatui::layout::Rect,
    content: Vec<Line<'static>>,
    keymap_line: Line<'static>,
) {
    let terminal_width = terminal_area.width;
    let terminal_height = terminal_area.height;
    let max_w = terminal_width.saturating_sub(4).max(1);
    let dialog_w = max_w.min(80).max(max_w.min(40));
    let content_h = content.len();
    let dialog_h = ((content_h + 3) as u16).min(terminal_height.saturating_sub(2));
    let dialog = ratatui::layout::Rect::new(
        (terminal_width.saturating_sub(dialog_w)) / 2,
        (terminal_height.saturating_sub(dialog_h)) / 2,
        dialog_w,
        dialog_h,
    );

    frame.render_widget(Clear, dialog);
    let block = Block::bordered().border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(block.clone(), dialog);
    let inner = block.inner(dialog);

    if inner.height > 0 && inner.width > 0 {
        let inner_h = inner.height as usize;
        let content_capacity = inner_h.saturating_sub(1);
        let lines_to_write: Vec<Line<'static>> = if content.len() <= content_capacity {
            content
        } else {
            let keep = content_capacity.saturating_sub(1);
            let mut truncated: Vec<Line<'static>> =
                content.into_iter().take(keep).collect();
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
