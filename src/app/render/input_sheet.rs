use super::*;

impl App {
    pub(super) fn input_sheet_content(&self, width: u16) -> Vec<Line<'static>> {
        let inner_width = (width as usize).saturating_sub(4).max(1);
        let placeholder = "type to agents...";
        let (text, text_style) = if self.input_buffer.is_empty() {
            (
                placeholder.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            (self.input_buffer.clone(), Style::default().fg(Color::White))
        };

        let mut wrapped = wrap_input(&text, inner_width);
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }

        let cursor_pos = {
            let target = if self.input_buffer.is_empty() {
                0
            } else {
                self.input_cursor.min(self.input_buffer.chars().count())
            };
            let mut acc = 0usize;
            let mut found = (wrapped.len().saturating_sub(1), 0usize);
            for (idx, chunk) in wrapped.iter().enumerate() {
                let chunk_len = chunk.chars().count();
                if target <= acc + chunk_len {
                    found = (idx, target - acc);
                    break;
                }
                acc += chunk_len;
            }
            found
        };

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            "> ",
            Style::default().fg(Color::DarkGray),
        )));

        for (idx, chunk) in wrapped.iter().enumerate() {
            let show_cursor_here = idx == cursor_pos.0;
            let split_col = if show_cursor_here { cursor_pos.1 } else { 0 };

            if show_cursor_here {
                let byte = chunk
                    .char_indices()
                    .nth(split_col)
                    .map(|(i, _)| i)
                    .unwrap_or(chunk.len());
                let (left, right) = (&chunk[..byte], &chunk[byte..]);
                lines.push(Line::from(vec![
                    Span::styled(left.to_string(), text_style),
                    Span::styled(
                        "▌".to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ),
                    Span::styled(right.to_string(), text_style),
                ]));
            } else {
                lines.push(Line::from(Span::styled(chunk.clone(), text_style)));
            }
        }

        lines
    }
}
