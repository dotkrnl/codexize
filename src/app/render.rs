use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use chrono::Offset;
use crate::state::{NodeStatus, Phase};

use super::{
    App, chat_widget,
    models::{vendor_color, vendor_prefix, vendor_tag},
    state::ModelRefreshState,
};
use crate::selection::VendorKind;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

struct PipelineWidget<'a> {
    app: &'a App,
}

impl Widget for PipelineWidget<'_> {
    fn render(self, area: ratatui::layout::Rect, buf: &mut Buffer) {
        let block = Block::default().title("Pipeline").borders(Borders::ALL);
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let local_offset = chrono::Local::now().fixed_offset().offset().fix();
        let expanded_count = self
            .app
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| self.app.is_expanded(*i))
            .count();
        let header_rows = self.app.nodes.len();
        let body_avail = (inner.height as usize).saturating_sub(header_rows);
        let per_stage = if expanded_count == 0 {
            0
        } else {
            (body_avail / expanded_count).max(3)
        };

        let bottom_y = inner.y.saturating_add(inner.height);
        let mut cursor_y = inner.y;

        for (index, node) in self.app.nodes.iter().enumerate() {
            if cursor_y >= bottom_y {
                break;
            }

            let expanded = self.app.is_expanded(index);
            let header = self.app.node_header(index, expanded, node);
            buf.set_line(inner.x, cursor_y, &header, inner.width);
            cursor_y += 1;
            if cursor_y >= bottom_y {
                break;
            }

            if expanded {
                let remaining = (bottom_y - cursor_y) as usize;
                let body_height = per_stage.min(remaining) as u16;
                if body_height == 0 {
                    continue;
                }

                let body_area = ratatui::layout::Rect {
                    x: inner.x,
                    y: cursor_y,
                    width: inner.width,
                    height: body_height,
                };

                self.app
                    .render_stage_body(body_area, buf, &local_offset, index);
                cursor_y += body_height;
            }
        }
    }
}

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
    // Normalise relative to the column max so the top entry always reads as
    // full green, regardless of absolute magnitude.
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

fn probability_span(label: &str, pct: u8, max_pct: u8) -> Span<'static> {
    let mut style = Style::default().fg(probability_color(pct, max_pct));
    if max_pct > 0 && pct == max_pct {
        style = style.add_modifier(Modifier::BOLD);
    }
    Span::styled(format!("{}{:>2}", label, pct), style)
}

fn spinner_frame(count: usize) -> &'static str {
    SPINNER[count % SPINNER.len()]
}

/// Hard-wrap the input text into lines of at most `width` chars, preferring
/// word boundaries when the line has any spaces. Preserves explicit newlines.
fn wrap_input(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        for word in raw_line.split_inclusive(' ') {
            let word_len = word.chars().count();
            if current_len + word_len <= width {
                current.push_str(word);
                current_len += word_len;
                continue;
            }
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                current_len = 0;
            }
            if word_len <= width {
                current.push_str(word);
                current_len = word_len;
            } else {
                let mut remaining = word;
                while remaining.chars().count() > width {
                    let split_at = remaining
                        .char_indices()
                        .nth(width)
                        .map(|(i, _)| i)
                        .unwrap_or(remaining.len());
                    out.push(remaining[..split_at].to_string());
                    remaining = &remaining[split_at..];
                }
                if !remaining.is_empty() {
                    current.push_str(remaining);
                    current_len = remaining.chars().count();
                }
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

pub fn sanitize_live_summary(text: &str) -> String {
    let stripped = strip_ansi_codes(text);
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(500).collect()
}

impl App {
    pub(super) fn draw(&mut self, frame: &mut Frame<'_>) {
        let model_height = (self.models.len() + self.quota_errors.len()).max(1) as u16 + 2;
        let root = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(8),
                Constraint::Length(model_height),
            ])
            .split(frame.area());

        self.body_inner_height = root[1].height.saturating_sub(2) as usize;
        self.body_inner_width = root[1].width.saturating_sub(2) as usize;
        self.clamp_scroll();

        frame.render_widget(self.header(), root[0]);
        frame.render_widget(PipelineWidget { app: self }, root[1]);
        frame.render_widget(self.model_strip(), root[2]);
    }

    fn header(&self) -> Paragraph<'_> {
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Codexize",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" #{} ", self.state.session_id)),
            Span::styled(
                format!("[{}]", self.state.current_phase.label()),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" | "),
            Span::raw(format!(
                "{}:{} {}",
                self.tmux.session_name, self.tmux.window_index, self.tmux.window_name
            )),
            Span::styled(
                {
                    let e = if self.editable_artifact().is_some() {
                        " e"
                    } else {
                        ""
                    };
                    let show_n = self.state.current_phase == Phase::SpecReviewPaused
                        || (self.state.current_phase == Phase::SpecReviewRunning
                            && self.state.agent_error.is_some());
                    let n = if show_n { " n" } else { "" };
                    format!(" | Up/Down Space Enter t PgUp/PgDn b{e}{n} q")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]))
    }

    fn render_stage_body(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut Buffer,
        local_offset: &chrono::FixedOffset,
        index: usize,
    ) {
        let node = &self.nodes[index];
        let run_id = node.run_id.or(node.leaf_run_id);
        if let Some(id) = run_id {
            if let Some(run) = self.state.agent_runs.iter().find(|r| r.id == id) {
                let msgs: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|m| m.run_id == id)
                    .cloned()
                    .collect();
                let scroll_offset = self
                    .stage_scroll_for(index)
                    .and_then(|(_, stored)| stored)
                    .unwrap_or(usize::MAX);
                let widget = chat_widget::ChatWidget::new(
                    &msgs,
                    run,
                    scroll_offset,
                    *local_offset,
                    self.agent_line_count,
                );
                widget.render(area, buf);
                return;
            }
        }

        // If there is no run to render, fall back to the compact body content.
        let body = self.node_body_with_offset(index, area.width as usize, local_offset);
        for (i, line) in body.iter().take(area.height as usize).enumerate() {
            let y = area.y.saturating_add(i as u16);
            buf.set_line(area.x, y, line, area.width);
        }
    }

    fn model_strip(&self) -> Paragraph<'static> {
        let mut vendor_order: Vec<VendorKind> = Vec::new();
        let mut by_vendor: std::collections::BTreeMap<
            VendorKind,
            Vec<&crate::selection::ModelStatus>,
        > = std::collections::BTreeMap::new();
        for model in &self.models {
            if !vendor_order.contains(&model.vendor) {
                vendor_order.push(model.vendor);
            }
            by_vendor.entry(model.vendor).or_default().push(model);
        }

        let total_idea: f64 = self.models.iter().map(|m| m.idea_weight).sum();
        let total_planning: f64 = self.models.iter().map(|m| m.planning_weight).sum();
        let total_build: f64 = self.models.iter().map(|m| m.build_weight).sum();
        let total_review: f64 = self.models.iter().map(|m| m.review_weight).sum();

        let max_idea = self
            .models
            .iter()
            .map(|m| probability_percent(m.idea_weight, total_idea))
            .max()
            .unwrap_or(0);
        let max_planning = self
            .models
            .iter()
            .map(|m| probability_percent(m.planning_weight, total_planning))
            .max()
            .unwrap_or(0);
        let max_build = self
            .models
            .iter()
            .map(|m| probability_percent(m.build_weight, total_build))
            .max()
            .unwrap_or(0);
        let max_review = self
            .models
            .iter()
            .map(|m| probability_percent(m.review_weight, total_review))
            .max()
            .unwrap_or(0);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for vendor in &vendor_order {
            let tag = vendor_tag(*vendor);
            let tag_color = vendor_color(*vendor);
            let prefix = vendor_prefix(*vendor);
            let models = &by_vendor[vendor];

            for (i, model) in models.iter().enumerate() {
                let short_name = model
                    .name
                    .strip_prefix(prefix)
                    .unwrap_or(&model.name)
                    .to_string();

                let stupid_level = model
                    .stupid_level
                    .map(|v| format!("{v:>2}"))
                    .unwrap_or_else(|| " -".to_string());
                let quota = model
                    .quota_percent
                    .map(|v| format!("{v:>3}%"))
                    .unwrap_or_else(|| " --%".to_string());

                let tag_span = if i == 0 {
                    Span::styled(
                        format!("{:<8}", tag),
                        Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("        ")
                };

                let idea_pct = probability_percent(model.idea_weight, total_idea);
                let planning_pct = probability_percent(model.planning_weight, total_planning);
                let build_pct = probability_percent(model.build_weight, total_build);
                let review_pct = probability_percent(model.review_weight, total_review);

                lines.push(Line::from(vec![
                    tag_span,
                    Span::styled(
                        format!("{:<28}", short_name),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(stupid_level, Style::default().fg(Color::Yellow)),
                    Span::raw("  "),
                    Span::styled(quota, Style::default().fg(Color::Green)),
                    Span::raw("  "),
                    probability_span("I", idea_pct, max_idea),
                    Span::raw(" "),
                    probability_span("P", planning_pct, max_planning),
                    Span::raw(" "),
                    probability_span("B", build_pct, max_build),
                    Span::raw(" "),
                    probability_span("R", review_pct, max_review),
                ]));
            }
        }

        for err in &self.quota_errors {
            let tag = vendor_tag(err.vendor);
            let msg = if err.message.len() > 60 {
                format!("{}...", &err.message[..60])
            } else {
                err.message.clone()
            };
            let retry_in = match &self.model_refresh {
                ModelRefreshState::Idle(at) => {
                    let elapsed = at.elapsed();
                    let due = self.quota_retry_delay;
                    if elapsed < due {
                        let secs = (due - elapsed).as_secs();
                        format!(" — retry in {}m{}s", secs / 60, secs % 60)
                    } else {
                        " — retrying...".to_string()
                    }
                }
                ModelRefreshState::Fetching { .. } => " — retrying now".to_string(),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  ⚠ {:<6}  ", tag),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{msg}{retry_in}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        Paragraph::new(lines).block(Block::default().title("Models").borders(Borders::ALL))
    }

    fn node_header(
        &self,
        index: usize,
        expanded: bool,
        node: &crate::state::Node,
    ) -> Line<'static> {
        let marker = if expanded { "▾" } else { "▸" };
        let is_current = index == super::tree::current_node_index(&self.nodes);
        let style = if index == self.selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::raw(format!("{marker} ")),
            Span::raw(node.label.clone()),
            Span::raw(" | "),
            Span::styled(node.status.label(), node.status.style()),
            Span::raw(" | "),
            Span::styled(node.summary.clone(), Style::default().fg(Color::Gray)),
        ];

        if self.confirm_back && is_current {
            spans.push(Span::styled(
                "  [b again to go back and clean up — any other key to cancel]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }

        Line::from(spans).style(style)
    }

    pub(super) fn node_body(&self, index: usize) -> Vec<Line<'static>> {
        let width = self.body_inner_width.max(1);
        let local_offset = chrono::Local::now().fixed_offset().offset().fix();
        self.node_body_with_offset(index, width, &local_offset)
    }

    fn node_body_with_offset(
        &self,
        index: usize,
        available_width: usize,
        local_offset: &chrono::FixedOffset,
    ) -> Vec<Line<'static>> {
        let node = &self.nodes[index];
        let run_id = node.run_id.or(node.leaf_run_id);
        if let Some(id) = run_id {
            if let Some(run) = self.state.agent_runs.iter().find(|r| r.id == id) {
                let msgs: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|m| m.run_id == id)
                    .cloned()
                    .collect();
                return chat_widget::message_lines(
                    &msgs,
                    run,
                    local_offset,
                    self.agent_line_count,
                    available_width,
                );
            }
        }
        self.render_compact_node(node, index)
    }

    fn render_compact_node(&self, node: &crate::state::Node, index: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if node.status == NodeStatus::Running && self.window_launched {
            let spin = spinner_frame(self.agent_line_count);
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    spin,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  running · {} lines", self.agent_line_count),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        if !node.children.is_empty() {
            lines.push(Line::from(""));
            for child in &node.children {
                lines.push(Line::from(vec![
                    Span::styled("  ── ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} ", child.label),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({})", child.status.label()), child.status.style()),
                    Span::styled(" ──", Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        // Input box for Idea stage
        if node.label == "Idea" && node.status == NodeStatus::WaitingUser {
            let active = self.input_mode && index == self.selected;
            let frame_color = if active {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            let width = 64usize;
            lines.push(Line::from(""));
            let label = if active { " working " } else { " input " };
            let fill = width.saturating_sub(label.len() + 2);
            let top = format!("  ╭{label}{}╮", "─".repeat(fill));
            lines.push(Line::from(Span::styled(
                top,
                Style::default().fg(frame_color),
            )));
            let placeholder = "describe what you want to build";
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
            let inner_width = width.saturating_sub(4);
            let mut wrapped = wrap_input(&text, inner_width);
            if wrapped.is_empty() {
                wrapped.push(String::new());
            }
            for (idx, chunk) in wrapped.iter().enumerate() {
                let is_last = idx + 1 == wrapped.len();
                let cursor = if active && is_last { "▌" } else { "" };
                let visible_len = chunk.chars().count() + cursor.chars().count();
                let padding = inner_width.saturating_sub(visible_len);
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(frame_color)),
                    Span::styled(chunk.clone(), text_style),
                    Span::styled(
                        cursor.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ),
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
            let bottom = format!("  ╰{}{hint}╯", "─".repeat(fill));
            lines.push(Line::from(Span::styled(
                bottom,
                Style::default().fg(frame_color),
            )));
        }
        lines
    }

}
