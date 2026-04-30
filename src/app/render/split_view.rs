use super::*;
use crate::app::chat_widget::ChatWidget;
use crate::app::clock::WallClock;
use crate::app::split::SplitTarget;
use crate::state::NodeStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub(super) struct SplitWidget<'a> {
    pub(super) app: &'a App,
}

impl Widget for SplitWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let Some(target) = self.app.split_target else {
            return;
        };

        match target {
            SplitTarget::Run(run_id) => self.render_run_split(run_id, area, buf),
            SplitTarget::Idea => self.render_idea_split(area, buf),
        }
    }
}

impl SplitWidget<'_> {
    fn render_run_split(&self, run_id: u64, area: Rect, buf: &mut Buffer) {
        let Some(run) = self.app.state.agent_runs.iter().find(|r| r.id == run_id) else {
            return;
        };

        let msgs: Vec<_> = self
            .app
            .messages
            .iter()
            .filter(|m| m.run_id == run_id)
            .filter(|m| {
                m.kind.visible_with_filters(
                    run.modes.interactive || self.app.state.show_noninteractive_texts,
                    self.app.state.show_thinking_texts,
                )
            })
            .cloned()
            .collect();

        // Resolve the selected row to check if we should suppress container
        // placeholders or show the leaf tail.
        let suppressed_container_runs = self
            .app
            .visible_live_summary_tail_runs(area.height as usize, self.app.viewport_top);

        let running_tail = self.app.running_tail_for_row(
            self.app.selected,
            run,
            &WallClock::new(),
            &suppressed_container_runs,
        );

        let local_offset = chrono::Local::now().fixed_offset().offset().fix();

        ChatWidget::new(
            &msgs,
            run,
            self.app.split_scroll_offset,
            local_offset,
            running_tail.map(|t| t.line),
        )
        .render(area, buf);
    }

    fn render_idea_split(&self, area: Rect, buf: &mut Buffer) {
        let node = self.app.node_for_row(self.app.selected);
        let is_waiting = node.is_some_and(|n| n.status == NodeStatus::WaitingUser);

        if is_waiting {
            self.render_idea_input(area, buf);
        } else if let Some(idea) = self.app.state.idea_text.as_deref() {
            self.render_idea_captured(idea, area, buf);
        }
    }

    fn render_idea_captured(&self, idea: &str, area: Rect, buf: &mut Buffer) {
        let width = (area.width as usize).clamp(20, 80);
        let inner_width = width.saturating_sub(4);
        let label = " idea ";
        let fill = width.saturating_sub(label.len() + 2);

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("╭{label}{}╮", "─".repeat(fill)),
            Style::default().fg(Color::DarkGray),
        )));

        for chunk in wrap_input(idea, inner_width) {
            let padding = inner_width.saturating_sub(chunk.chars().count());
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::styled(chunk, Style::default().fg(Color::White)),
                Span::raw(" ".repeat(padding)),
                Span::styled(" │", Style::default().fg(Color::DarkGray)),
            ]));
        }

        lines.push(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
            Style::default().fg(Color::DarkGray),
        )));

        // Simple vertical centering within the split area
        let total_h = lines.len() as u16;
        let start_y = area.y + area.height.saturating_sub(total_h) / 2;
        let start_x = area.x + area.width.saturating_sub(width as u16) / 2;

        for (i, line) in lines.into_iter().enumerate() {
            let y = start_y + i as u16;
            if y < area.y + area.height {
                buf.set_line(start_x, y, &line, width as u16);
            }
        }
    }

    fn render_idea_input(&self, area: Rect, buf: &mut Buffer) {
        let active = self.app.input_mode;
        let frame_color = if active {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        let width = (area.width as usize).clamp(20, 80);
        let label = if active { " working " } else { " input " };
        let fill = width.saturating_sub(label.len() + 2);

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("╭{label}{}╮", "─".repeat(fill)),
            Style::default().fg(frame_color),
        )));

        let placeholder = "describe your idea...";
        let (text, text_style) = if self.app.input_buffer.is_empty() {
            (
                placeholder.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            (
                self.app.input_buffer.clone(),
                Style::default().fg(Color::White),
            )
        };

        let inner_width = width.saturating_sub(4);
        let mut wrapped = wrap_input(&text, inner_width);
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }

        let cursor_pos = if active {
            let target = if self.app.input_buffer.is_empty() {
                0
            } else {
                self.app
                    .input_cursor
                    .min(self.app.input_buffer.chars().count())
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
            Some(found)
        } else {
            None
        };

        for (idx, chunk) in wrapped.iter().enumerate() {
            let show_cursor_here = cursor_pos.is_some_and(|(line, _)| line == idx);
            let split_col = cursor_pos
                .filter(|(line, _)| *line == idx)
                .map(|(_, col)| col)
                .unwrap_or(0);
            let (left, right) = if show_cursor_here {
                let byte = chunk
                    .char_indices()
                    .nth(split_col)
                    .map(|(i, _)| i)
                    .unwrap_or(chunk.len());
                (&chunk[..byte], &chunk[byte..])
            } else {
                (chunk.as_str(), "")
            };
            let cursor = if show_cursor_here { "▌" } else { "" };
            let visible_len = chunk.chars().count() + cursor.chars().count();
            let padding = inner_width.saturating_sub(visible_len);
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(frame_color)),
                Span::styled(left.to_string(), text_style),
                Span::styled(
                    cursor.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::styled(right.to_string(), text_style),
                Span::raw(" ".repeat(padding)),
                Span::styled(" │", Style::default().fg(frame_color)),
            ]));
        }

        let hint = if active {
            " Enter: submit · Esc: cancel "
        } else {
            " Enter to type "
        };
        let fill = width.saturating_sub(hint.len() + 2);
        lines.push(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(fill) + hint),
            Style::default().fg(frame_color),
        )));

        // Center vertically
        let total_h = lines.len() as u16;
        let start_y = area.y + area.height.saturating_sub(total_h) / 2;
        let start_x = area.x + area.width.saturating_sub(width as u16) / 2;

        for (i, line) in lines.into_iter().enumerate() {
            let y = start_y + i as u16;
            if y < area.y + area.height {
                buf.set_line(start_x, y, &line, width as u16);
            }
        }
    }
}
