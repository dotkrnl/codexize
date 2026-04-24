use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::{
    selection::VendorKind,
    state::Phase,
};

use super::{
    App, PREVIEW_LINES,
    models::{vendor_color, vendor_prefix, vendor_tag},
    state::{ModelRefreshState, PipelineSection, SectionStatus},
};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

fn sanitize_live_summary(text: &str) -> String {
    let stripped = strip_ansi_codes(text);
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(500).collect()
}

fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if !current.is_empty() && current.len() + 1 + word.len() > width {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
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
        self.clamp_scroll();

        frame.render_widget(self.header(), root[0]);
        frame.render_widget(self.pipeline_view(), root[1]);
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
                    let e = if self.editable_artifact().is_some() { " e" } else { "" };
                    let show_n = self.state.current_phase == Phase::SpecReviewPaused
                        || (self.state.current_phase == Phase::SpecReviewRunning
                            && self.state.agent_error.is_some());
                    let n = if show_n { " n" } else { "" };
                    format!(" | Up/Down Enter t PgUp/PgDn b{e}{n} q")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]))
    }

    fn pipeline_view(&self) -> Paragraph<'static> {
        let mut lines = Vec::new();
        let current = self.current_section();
        let selected_limit = self.selected_body_limit();
        let mut selected_header_line = 0usize;

        for (index, section) in self.sections.iter().enumerate() {
            let expanded = self.is_expanded(index);
            if index == self.selected {
                selected_header_line = lines.len();
            }

            lines.push(self.section_header(index, expanded, section));

            if expanded {
                let body_lines = self.section_body(index);
                if index == self.selected {
                    let visible = self.visible_selected_body(&body_lines, selected_limit, index);
                    lines.extend(visible);
                } else {
                    lines.extend(self.preview_body(&body_lines));
                }
            } else if index > current && section.status == SectionStatus::Pending {
                // keep pending future phases terse
            }
        }

        let viewport = self.body_inner_height;
        let max_scroll = lines.len().saturating_sub(viewport);
        let scroll = selected_header_line.saturating_sub(1).min(max_scroll) as u16;

        Paragraph::new(lines)
            .block(Block::default().title("Pipeline").borders(Borders::ALL))
            .scroll((scroll, 0))
    }

    fn model_strip(&self) -> Paragraph<'static> {
        let mut vendor_order: Vec<VendorKind> = Vec::new();
        let mut by_vendor: std::collections::BTreeMap<VendorKind, Vec<&crate::selection::ModelStatus>> =
            std::collections::BTreeMap::new();
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

    fn section_header(
        &self,
        index: usize,
        expanded: bool,
        section: &PipelineSection,
    ) -> Line<'static> {
        let marker = if expanded { "v" } else { ">" };
        let is_current = index == super::sections::current_section_index(&self.sections);
        let style = if index == self.selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::raw(format!("{marker} ")),
            Span::raw(section.name.clone()),
            Span::raw(" | "),
            Span::styled(section.status.label(), section.status.style()),
            Span::raw(" | "),
            Span::styled(section.summary.clone(), Style::default().fg(Color::Gray)),
        ];

        if self.confirm_back && is_current {
            spans.push(Span::styled(
                "  [b again to go back and clean up — any other key to cancel]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }

        Line::from(spans).style(style)
    }

    pub(super) fn section_body(&self, index: usize) -> Vec<Line<'static>> {
        let section = &self.sections[index];
        let mut lines = section
            .events
            .iter()
            .map(|event| {
                Line::from(vec![
                    Span::styled("  - ", Style::default().fg(Color::DarkGray)),
                    Span::raw(event.clone()),
                ])
            })
            .collect::<Vec<_>>();

        if section.status == SectionStatus::Running && self.window_launched {
            let phase_key = match self.state.current_phase {
                Phase::BrainstormRunning => Some("brainstorm"),
                Phase::SpecReviewRunning => Some("spec-review"),
                Phase::PlanningRunning => Some("planning"),
                Phase::ShardingRunning => Some("sharding"),
                _ => None,
            };
            if let Some(key) = phase_key {
                let model_label = self.state.phase_models.get(key)
                    .map(|pm| format!("{} ({})", pm.model, pm.vendor))
                    .unwrap_or_else(|| "unknown model".to_string());
                let spin = spinner_frame(self.agent_line_count);
                lines.insert(0, Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(spin, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::styled(model_label, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!(" · {} lines", self.agent_line_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }

        // Live summary for currently running agent
        if section.status == SectionStatus::Running
            && self.window_launched
            && !self.live_summary.is_empty()
        {
            let sanitized = sanitize_live_summary(&self.live_summary);
            let wrapped = hard_wrap(&sanitized, 80);
            for (i, line) in wrapped.into_iter().enumerate() {
                lines.insert(
                    i,
                    Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled("⦿ ", Style::default().fg(Color::Green)),
                        Span::raw(line),
                    ]),
                );
            }
        }

        if section.events.is_empty() {
            lines.push(Line::from(Span::styled(
                "  - no events yet",
                Style::default().fg(Color::DarkGray),
            )));
        }

        if !section.transcript.is_empty() {
            if self.transcript_open.contains(&index) {
                lines.push(Line::from(Span::styled(
                    "  transcript",
                    Style::default().fg(Color::Magenta),
                )));
                lines.extend(section.transcript.iter().map(|line| {
                    Line::from(vec![
                        Span::styled("    ", Style::default().fg(Color::DarkGray)),
                        Span::raw(line.clone()),
                    ])
                }));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("  [t] transcript hidden ({})", section.transcript.len()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        if let Some(placeholder) = &section.input_placeholder {
            let active = self.input_mode && index == self.selected;
            let frame_color = if active { Color::Yellow } else { Color::DarkGray };
            let width = 64usize;

            lines.push(Line::from(""));

            let label = if active { " typing " } else { " input " };
            let fill = width.saturating_sub(label.len() + 2);
            let top = format!(
                "  ╭{label}{}╮",
                "─".repeat(fill),
            );
            lines.push(Line::from(Span::styled(top, Style::default().fg(frame_color))));

            let (text, text_style) = if self.input_buffer.is_empty() {
                (placeholder.clone(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))
            } else {
                (self.input_buffer.clone(), Style::default().fg(Color::White))
            };
            let cursor = if active { "▌" } else { "" };
            let content_visible_len = text.chars().count() + cursor.chars().count();
            let inner_width = width.saturating_sub(2);
            let padding = inner_width.saturating_sub(content_visible_len);
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(frame_color)),
                Span::styled(text, text_style),
                Span::styled(
                    cursor.to_string(),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::raw(" ".repeat(padding.saturating_sub(2))),
                Span::styled(" │", Style::default().fg(frame_color)),
            ]));

            let hint = if active { " Enter: submit · Esc: cancel " } else { " Enter to type " };
            let fill = width.saturating_sub(hint.len() + 2);
            let bottom = format!("  ╰{}{hint}╯", "─".repeat(fill));
            lines.push(Line::from(Span::styled(bottom, Style::default().fg(frame_color))));
        }

        // Historical attempts
        if !section.attempts.is_empty() {
            lines.push(Line::from(""));
            for attempt in &section.attempts {
                lines.push(Line::from(vec![
                    Span::styled("  ── ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} ", attempt.label),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("({})", attempt.status.label()),
                        attempt.status.style(),
                    ),
                    Span::styled(" ──", Style::default().fg(Color::DarkGray)),
                ]));
                if !attempt.summary.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("    ", Style::default().fg(Color::DarkGray)),
                        Span::styled(attempt.summary.clone(), Style::default().fg(Color::Gray)),
                    ]));
                }
                for event in &attempt.events {
                    lines.push(Line::from(vec![
                        Span::styled("      - ", Style::default().fg(Color::DarkGray)),
                        Span::raw(event.clone()),
                    ]));
                }
                if !attempt.live_summary.is_empty() {
                    lines.push(Line::from(vec![Span::styled(
                        "    Final Summary: ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )]));
                    let sanitized = sanitize_live_summary(&attempt.live_summary);
                    let wrapped = hard_wrap(&sanitized, 80);
                    for line in wrapped {
                        lines.push(Line::from(vec![
                            Span::styled("      ", Style::default()),
                            Span::raw(line),
                        ]));
                    }
                }
            }
        }

        lines
    }

    fn visible_selected_body(
        &self,
        body_lines: &[Line<'static>],
        limit: usize,
        index: usize,
    ) -> Vec<Line<'static>> {
        if body_lines.is_empty() {
            return Vec::new();
        }

        let max_offset = body_lines.len().saturating_sub(limit);
        let offset = self.section_scroll_offset(index, body_lines.len(), limit);
        let end = (offset + limit).min(body_lines.len());
        let mut visible = Vec::new();

        if offset > 0 {
            visible.push(Line::from(Span::styled(
                "  ... older ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        visible.extend(body_lines[offset..end].iter().cloned());

        if offset < max_offset {
            visible.push(Line::from(Span::styled(
                "  ... newer ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        visible
    }

    fn preview_body(&self, body_lines: &[Line<'static>]) -> Vec<Line<'static>> {
        if body_lines.is_empty() {
            return Vec::new();
        }

        let start = body_lines.len().saturating_sub(PREVIEW_LINES);
        let mut visible = Vec::new();

        if start > 0 {
            visible.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        visible.extend(body_lines[start..].iter().cloned());
        visible
    }
}
