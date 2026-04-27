use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::state::{NodeStatus, Phase};
use chrono::Offset;

#[cfg(test)]
use super::state::ModelRefreshState;
use super::{
    App, ModalKind, StageId, chat_widget,
    models::{vendor_color, vendor_prefix, vendor_tag},
};
use crate::model_names;
use crate::selection::{CachedModel, VendorKind, display::visible_models, ranking::VersionIndex};
use crate::tui::wrap_input;

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

        // Build the full linear content stream: each visible row contributes one
        // header line and, if expanded with a transcript, its full natural body.
        // Sections never share or compete for height; overflow is handled by the
        // pipeline-level `viewport_top` scroll instead.
        let mut lines: Vec<Line<'static>> = Vec::new();
        for index in 0..self.app.visible_rows.len() {
            let Some(node) = self.app.node_for_row(index) else {
                continue;
            };
            let expanded = self.app.is_expanded(index);
            lines.push(self.app.node_header(index, expanded, node));
            if expanded && self.app.is_expanded_body(index) {
                lines.extend(self.app.node_body(index));
            }
        }

        let area_h = inner.height as usize;
        let viewport_top = self
            .app
            .viewport_top
            .min(lines.len().saturating_sub(area_h));
        let end = (viewport_top + area_h).min(lines.len());
        for (offset, line) in lines[viewport_top..end].iter().enumerate() {
            buf.set_line(inner.x, inner.y + offset as u16, line, inner.width);
        }

        // "↓ N new" badge centered along the bottom of the pipeline frame
        // when tail-follow is detached and messages have arrived since.
        let unread = self.app.unread_below_count();
        let at_bottom = self.app.viewport_top >= self.app.max_viewport_top();
        let viewport_bottom = viewport_top + area_h;
        let unread_below_viewport = self
            .app
            .first_unread_rendered_line()
            .map(|line| line >= viewport_bottom)
            .unwrap_or(!at_bottom);
        if unread > 0 && unread_below_viewport && area.height >= 1 {
            let label = format!(" ↓ {unread} new ");
            let label_w = label.chars().count() as u16;
            if label_w + 2 <= area.width {
                let x = area.x + (area.width.saturating_sub(label_w)) / 2;
                let y = area.y + area.height - 1;
                let span = Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                );
                buf.set_line(x, y, &Line::from(span), label_w);
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

fn probability_span(label: &str, pct: u8, max_pct: u8, is_top_rank: bool) -> Span<'static> {
    let mut style = Style::default().fg(probability_color(pct, max_pct));
    if is_top_rank {
        style = style.add_modifier(Modifier::BOLD);
    }
    Span::styled(format!("{}{:>2}", label, pct), style)
}

/// Span used when the vendor's quota fetch failed: shows "--" in red so the
/// user sees that the probability data is unavailable rather than zero.
fn probability_unavailable_span(label: &str) -> Span<'static> {
    Span::styled(format!("{}--", label), Style::default().fg(Color::Red))
}

fn spinner_frame(count: usize) -> &'static str {
    SPINNER[count % SPINNER.len()]
}

fn model_strip_height(models: &[CachedModel], versions: &VersionIndex) -> u16 {
    let visible = visible_models(models, versions);
    let has_provenance = visible.iter().any(|name| {
        models
            .iter()
            .find(|m| &m.name == name)
            .is_some_and(|m| !m.axis_provenance.is_empty())
    });
    let visible_count = visible.len() as u16;
    if visible_count == 0 {
        2
    } else if has_provenance {
        visible_count * 2 + 2
    } else {
        visible_count + 2
    }
}

fn format_provenance_line(model: &CachedModel) -> Line<'static> {
    let mut parts: Vec<String> = model
        .axis_provenance
        .iter()
        .map(|(axis, label)| format!("{axis}: {label}"))
        .collect();
    parts.sort();
    let text = format!("        {}", parts.join("  "));
    Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
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

fn format_model_name_spans(
    short_name: &str,
    is_new: bool,
    target_width: usize,
) -> Vec<Span<'static>> {
    const SUFFIX: &str = " (new)";
    const ELLIPSIS: &str = "...";

    let display_name = if is_new {
        format!("{short_name}{SUFFIX}")
    } else {
        short_name.to_string()
    };
    let name_len = short_name.chars().count();
    let full_len = display_name.chars().count();
    let ellipsis_len = ELLIPSIS.chars().count();

    if full_len <= target_width {
        let pad = target_width.saturating_sub(full_len);
        let mut spans = vec![Span::styled(
            short_name.to_string(),
            Style::default().fg(Color::Cyan),
        )];
        if is_new {
            spans.push(Span::styled(SUFFIX, Style::default().fg(Color::DarkGray)));
        }
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans
    } else if target_width > ellipsis_len {
        let visible_chars = target_width.saturating_sub(ellipsis_len);
        let truncated_display: String = display_name.chars().take(visible_chars).collect();
        if is_new {
            let name_part: String = truncated_display.chars().take(name_len).collect();
            let suffix_part: String = truncated_display.chars().skip(name_len).collect();
            let mut spans = vec![Span::styled(name_part, Style::default().fg(Color::Cyan))];
            if !suffix_part.is_empty() {
                spans.push(Span::styled(
                    suffix_part,
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(ELLIPSIS, Style::default().fg(Color::DarkGray)));
            spans
        } else {
            let truncated = truncated_display;
            vec![
                Span::styled(truncated, Style::default().fg(Color::Cyan)),
                Span::styled(ELLIPSIS, Style::default().fg(Color::DarkGray)),
            ]
        }
    } else {
        vec![Span::styled(
            ELLIPSIS.chars().take(target_width).collect::<String>(),
            Style::default().fg(Color::DarkGray),
        )]
    }
}

impl App {
    pub(super) fn draw(&mut self, frame: &mut Frame<'_>) {
        let model_height = model_strip_height(&self.models, &self.versions);
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
        self.latch_visible_expansions();
        self.clamp_viewport();

        frame.render_widget(self.header(), root[0]);
        frame.render_widget(PipelineWidget { app: self }, root[1]);
        frame.render_widget(self.model_strip(root[2].width), root[2]);

        if let Some(modal) = self.active_modal() {
            self.render_modal(frame, modal);
        }
    }

    fn render_modal(&self, frame: &mut Frame<'_>, modal: ModalKind) {
        match modal {
            ModalKind::SkipToImpl => {
                render_skip_to_impl_modal(
                    frame,
                    self.state.skip_to_impl_rationale.as_deref(),
                    self.state.skip_to_impl_kind,
                );
            }
            ModalKind::GitGuard => {
                render_guard_decision_modal(frame, self.state.pending_guard_decision.as_ref());
            }
            ModalKind::SpecReviewPaused => {
                render_pause_modal(
                    frame,
                    "Spec review complete",
                    "[Enter] continue  [n] additional reviewer  [q] quit",
                );
            }
            ModalKind::PlanReviewPaused => {
                render_pause_modal(
                    frame,
                    "Plan review complete",
                    "[Enter] continue  [n] additional reviewer  [q] quit",
                );
            }
            ModalKind::StageError(stage_id) => {
                render_stage_error_modal(frame, stage_id, self.state.agent_error.as_deref());
            }
        }
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
                self.nodes
                    .get(self.current_node())
                    .map(|n| n.status.style())
                    .unwrap_or_default(),
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
                        || self.state.current_phase == Phase::PlanReviewPaused;
                    let n = if show_n { " n" } else { "" };
                    format!(" | Up/Down Space Enter t PgUp/PgDn b{e}{n} q")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]))
    }

    fn model_strip(&self, strip_width: u16) -> Paragraph<'static> {
        use crate::selection::{
            config::SelectionPhase, display::phase_rank, ranking::selection_probability,
        };

        // Fixed-width parts: vendor tag (8) + stupid (2) + "  " (2) + quota (4) +
        // "  " (2) + "Ixx Pxx Bxx Rxx" (15) + block borders (2) = 35.
        // The remaining width goes to the model name column.
        const FIXED_WIDTH: usize = 35;
        const MIN_NAME_WIDTH: usize = 8;
        const DEFAULT_NAME_WIDTH: usize = 28;
        let name_width = if strip_width as usize > FIXED_WIDTH + MIN_NAME_WIDTH {
            (strip_width as usize - FIXED_WIDTH).min(DEFAULT_NAME_WIDTH)
        } else {
            MIN_NAME_WIDTH
        };

        // Probabilities are computed on demand per phase and normalised against
        // the global total over every assembled model (not just the visible
        // subset) so that filtering doesn't artificially inflate percentages.
        let prob_for = |phase: SelectionPhase, model: &CachedModel| -> f64 {
            selection_probability(model, phase, &self.versions)
        };
        let total_for =
            |phase: SelectionPhase| -> f64 { self.models.iter().map(|m| prob_for(phase, m)).sum() };

        let total_idea = total_for(SelectionPhase::Idea);
        let total_planning = total_for(SelectionPhase::Planning);
        let total_build = total_for(SelectionPhase::Build);
        let total_review = total_for(SelectionPhase::Review);

        let idea_ranks = phase_rank(&self.models, SelectionPhase::Idea, &self.versions);
        let planning_ranks = phase_rank(&self.models, SelectionPhase::Planning, &self.versions);
        let build_ranks = phase_rank(&self.models, SelectionPhase::Build, &self.versions);
        let review_ranks = phase_rank(&self.models, SelectionPhase::Review, &self.versions);

        let visible = visible_models(&self.models, &self.versions);

        let mut vendor_order: Vec<VendorKind> = Vec::new();
        let mut by_vendor: std::collections::BTreeMap<VendorKind, Vec<&CachedModel>> =
            std::collections::BTreeMap::new();
        for model in self.models.iter().filter(|m| visible.contains(&m.name)) {
            if !vendor_order.contains(&model.vendor) {
                vendor_order.push(model.vendor);
            }
            by_vendor.entry(model.vendor).or_default().push(model);
        }
        for models in by_vendor.values_mut() {
            models.sort_by_key(|m| m.display_order);
        }

        let max_idea = self
            .models
            .iter()
            .map(|m| probability_percent(prob_for(SelectionPhase::Idea, m), total_idea))
            .max()
            .unwrap_or(0);
        let max_planning = self
            .models
            .iter()
            .map(|m| probability_percent(prob_for(SelectionPhase::Planning, m), total_planning))
            .max()
            .unwrap_or(0);
        let max_build = self
            .models
            .iter()
            .map(|m| probability_percent(prob_for(SelectionPhase::Build, m), total_build))
            .max()
            .unwrap_or(0);
        let max_review = self
            .models
            .iter()
            .map(|m| probability_percent(prob_for(SelectionPhase::Review, m), total_review))
            .max()
            .unwrap_or(0);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for vendor in &vendor_order {
            let tag = vendor_tag(*vendor);
            let tag_color = vendor_color(*vendor);
            let prefix = vendor_prefix(*vendor);
            let models = &by_vendor[vendor];

            for (i, model) in models.iter().enumerate() {
                let short_name = model_names::display_name_for_vendor(&model.name, prefix);

                let stupid_value: u8 = model.current_score.round().clamp(0.0, 99.0) as u8;
                let stupid_level = format!("{stupid_value:>2}");
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

                let idea_pct =
                    probability_percent(prob_for(SelectionPhase::Idea, model), total_idea);
                let planning_pct =
                    probability_percent(prob_for(SelectionPhase::Planning, model), total_planning);
                let build_pct =
                    probability_percent(prob_for(SelectionPhase::Build, model), total_build);
                let review_pct =
                    probability_percent(prob_for(SelectionPhase::Review, model), total_review);
                let vendor_failed = self
                    .quota_errors
                    .iter()
                    .any(|err| err.vendor == model.vendor);

                let name_spans =
                    format_model_name_spans(&short_name, model.fallback_from.is_some(), name_width);

                let mut line_spans = vec![tag_span];
                line_spans.extend(name_spans);
                let prob_i = if vendor_failed {
                    probability_unavailable_span("I")
                } else {
                    probability_span(
                        "I",
                        idea_pct,
                        max_idea,
                        idea_ranks.get(&model.name) == Some(&1),
                    )
                };
                let prob_p = if vendor_failed {
                    probability_unavailable_span("P")
                } else {
                    probability_span(
                        "P",
                        planning_pct,
                        max_planning,
                        planning_ranks.get(&model.name) == Some(&1),
                    )
                };
                let prob_b = if vendor_failed {
                    probability_unavailable_span("B")
                } else {
                    probability_span(
                        "B",
                        build_pct,
                        max_build,
                        build_ranks.get(&model.name) == Some(&1),
                    )
                };
                let prob_r = if vendor_failed {
                    probability_unavailable_span("R")
                } else {
                    probability_span(
                        "R",
                        review_pct,
                        max_review,
                        review_ranks.get(&model.name) == Some(&1),
                    )
                };
                // Both metrics share the probability gradient (red→yellow→green
                // on 0..100), where higher is better — for stupid_level a higher
                // score literally means "more clever", and for quota_percent a
                // higher value means "more headroom remaining".
                let stupid_color = probability_color(stupid_value, 100);
                let quota_color = match model.quota_percent {
                    Some(v) => probability_color(v, 100),
                    None => Color::DarkGray,
                };
                line_spans.extend(vec![
                    Span::styled(stupid_level, Style::default().fg(stupid_color)),
                    Span::raw("  "),
                    Span::styled(quota, Style::default().fg(quota_color)),
                    Span::raw("  "),
                    prob_i,
                    Span::raw(" "),
                    prob_p,
                    Span::raw(" "),
                    prob_b,
                    Span::raw(" "),
                    prob_r,
                ]);
                lines.push(Line::from(line_spans));
                if !model.axis_provenance.is_empty() {
                    lines.push(format_provenance_line(model));
                }
            }
        }

        Paragraph::new(lines).block(Block::default().title("Models").borders(Borders::ALL))
    }

    fn node_header(
        &self,
        index: usize,
        expanded: bool,
        node: &crate::state::Node,
    ) -> Line<'static> {
        let marker = if node.status == NodeStatus::Pending {
            " "
        } else if expanded {
            "▾"
        } else {
            "▸"
        };
        let is_current = index == self.current_row();
        let style = if index == self.selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::raw(format!(
                "{}{} ",
                " ".repeat(self.visible_rows[index].depth),
                marker
            )),
            Span::raw(node.label.clone()),
            Span::raw(" | "),
            Span::styled(node.status.label(), node.status.style()),
        ];
        // Only the Builder Loop carries useful per-stage progress in its
        // summary ("N of M tasks done"); the other stages emit narration like
        // "idea captured" that just clutters the title.
        if node.label == "Builder Loop" && !node.summary.is_empty() {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled(
                node.summary.clone(),
                Style::default().fg(Color::Gray),
            ));
        }

        if self.confirm_back && is_current {
            spans.push(Span::styled(
                "  [b again to go back and clean up — any other key to cancel]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
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
        let Some(node) = self.node_for_row(index) else {
            return Vec::new();
        };
        let run_id = node.run_id.or(node.leaf_run_id);
        if let Some(id) = run_id
            && let Some(run) = self.state.agent_runs.iter().find(|r| r.id == id)
        {
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
                self.spinner_tick,
                available_width,
            );
        }
        self.render_compact_node(node, index)
    }

    fn render_compact_node(&self, node: &crate::state::Node, index: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if node.status == NodeStatus::Running && self.window_launched {
            let spin = spinner_frame(self.spinner_tick);
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
        // Captured idea shown in the body (not the title)
        if node.label == "Idea"
            && node.status == NodeStatus::Done
            && let Some(idea) = self.state.idea_text.as_deref()
        {
            let width = 64usize;
            let inner_width = width.saturating_sub(4);
            let label = " idea ";
            let fill = width.saturating_sub(label.len() + 2);
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  ╭{label}{}╮", "─".repeat(fill)),
                Style::default().fg(Color::DarkGray),
            )));
            for chunk in wrap_input(idea, inner_width) {
                let padding = inner_width.saturating_sub(chunk.chars().count());
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(chunk, Style::default().fg(Color::White)),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(" │", Style::default().fg(Color::DarkGray)),
                ]));
            }
            lines.push(Line::from(Span::styled(
                format!("  ╰{}╯", "─".repeat(width.saturating_sub(2))),
                Style::default().fg(Color::DarkGray),
            )));
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
            // Map the char-index cursor onto (line, column) within the
            // wrapped chunks. Placeholder text is never editable, so the
            // cursor pins to 0 when the buffer is empty.
            let cursor_pos = if active {
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
                    Span::styled("  │ ", Style::default().fg(frame_color)),
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
            let bottom = format!("  ╰{}{hint}╯", "─".repeat(fill));
            lines.push(Line::from(Span::styled(
                bottom,
                Style::default().fg(frame_color),
            )));
        }
        lines
    }
}

fn render_skip_to_impl_modal(
    frame: &mut Frame<'_>,
    rationale: Option<&str>,
    kind: Option<crate::artifacts::SkipToImplKind>,
) {
    use crate::artifacts::SkipToImplKind;
    let area = frame.area();
    let modal_width = area.width.saturating_sub(8).clamp(30, 70);

    let is_nothing = kind == Some(SkipToImplKind::NothingToDo);
    let (header, accept_line, decline_line, title) = if is_nothing {
        (
            "The brainstorm agent found nothing to implement.",
            "[Y]/Enter  accept — mark session done",
            "[N]/Esc    decline — re-run brainstorm",
            "Nothing to implement?",
        )
    } else {
        (
            "The brainstorm agent proposes skipping directly to implementation.",
            "[Y]/Enter  accept — jump to implementation round 1",
            "[N]/Esc    decline — continue through spec review",
            "Skip to implementation?",
        )
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        header.to_string(),
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(""));
    let rationale_text = rationale
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no rationale provided)");
    lines.push(Line::from(vec![
        Span::styled("Rationale: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(rationale_text.to_string()),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        accept_line.to_string(),
        Style::default().fg(Color::Green),
    )));
    lines.push(Line::from(Span::styled(
        decline_line.to_string(),
        Style::default().fg(Color::Red),
    )));

    // Estimate wrapped line count so the accept/decline buttons are never clipped.
    let inner_width = modal_width.saturating_sub(2).max(1) as usize;
    let wrapped: u16 = lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width).max(1) as u16
            }
        })
        .sum();
    let desired_height = wrapped.saturating_add(2); // borders
    let modal_height = desired_height.min(area.height.saturating_sub(2)).max(6);

    let x = area.x + area.width.saturating_sub(modal_width) / 2;
    let y = area.y + area.height.saturating_sub(modal_height) / 2;
    let rect = ratatui::layout::Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    frame.render_widget(ratatui::widgets::Clear, rect);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph, rect);
}

fn render_guard_decision_modal(
    frame: &mut Frame<'_>,
    decision: Option<&crate::state::PendingGuardDecision>,
) {
    let area = frame.area();
    let modal_width = area.width.saturating_sub(8).clamp(30, 72);

    let (captured_short, current_short) = decision
        .map(|d| {
            let cap = d.captured_head.get(..7).unwrap_or(&d.captured_head);
            let cur = d.current_head.get(..7).unwrap_or(&d.current_head);
            (cap.to_string(), cur.to_string())
        })
        .unwrap_or_else(|| ("???????".to_string(), "???????".to_string()));

    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "An interactive agent advanced HEAD during a stage that must not commit.".to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Before: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(captured_short),
            Span::raw("  →  "),
            Span::styled("After: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(current_short),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "[R]/Enter  reset — discard commit and fail this run".to_string(),
            Style::default().fg(Color::Red),
        )),
        Line::from(Span::styled(
            "[K]        keep  — preserve commit and continue".to_string(),
            Style::default().fg(Color::Green),
        )),
        Line::from(Span::styled(
            "[Q]        quit".to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let inner_width = modal_width.saturating_sub(2).max(1) as usize;
    let wrapped: u16 = lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width).max(1) as u16
            }
        })
        .sum();
    let desired_height = wrapped.saturating_add(2);
    let modal_height = desired_height.min(area.height.saturating_sub(2)).max(6);

    let x = area.x + area.width.saturating_sub(modal_width) / 2;
    let y = area.y + area.height.saturating_sub(modal_height) / 2;
    let rect = ratatui::layout::Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    frame.render_widget(ratatui::widgets::Clear, rect);

    let block = Block::default()
        .title("Unauthorized commit detected")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph, rect);
}

fn stage_error_title(stage_id: StageId) -> &'static str {
    match stage_id {
        StageId::Brainstorm => "Brainstorm failed",
        StageId::SpecReview => "Spec review failed",
        StageId::Planning => "Planning failed",
        StageId::PlanReview => "Plan review failed",
        StageId::Sharding => "Sharding failed",
        StageId::Implementation => "Implementation failed",
        StageId::Review => "Review failed",
    }
}

fn render_pause_modal(frame: &mut Frame<'_>, title: &str, hint: &str) {
    let area = frame.area();
    let modal_width = area.width.saturating_sub(8).clamp(30, 70);

    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(Color::Green),
        )),
    ];

    let inner_width = modal_width.saturating_sub(2).max(1) as usize;
    let wrapped: u16 = lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width).max(1) as u16
            }
        })
        .sum();
    let desired_height = wrapped.saturating_add(2);
    let modal_height = desired_height.min(area.height.saturating_sub(2)).max(6);

    let x = area.x + area.width.saturating_sub(modal_width) / 2;
    let y = area.y + area.height.saturating_sub(modal_height) / 2;
    let rect = ratatui::layout::Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    frame.render_widget(ratatui::widgets::Clear, rect);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph, rect);
}

fn render_stage_error_modal(frame: &mut Frame<'_>, stage_id: StageId, error: Option<&str>) {
    let area = frame.area();
    let modal_width = area.width.saturating_sub(8).clamp(30, 70);

    let title = stage_error_title(stage_id);
    let error_text = error
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no error details)");
    let truncated: String = error_text.chars().take(300).collect();

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        truncated,
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(""));
    let mut hint = "[Enter] retry  [q] quit".to_string();
    if stage_id == StageId::Brainstorm {
        hint.push_str("  [e] edit idea");
    }
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::Green),
    )));

    let inner_width = modal_width.saturating_sub(2).max(1) as usize;
    let wrapped: u16 = lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width).max(1) as u16
            }
        })
        .sum();
    let desired_height = wrapped.saturating_add(2);
    let modal_height = desired_height.min(area.height.saturating_sub(2)).max(6);

    let x = area.x + area.width.saturating_sub(modal_width) / 2;
    let y = area.y + area.height.saturating_sub(modal_height) / 2;
    let rect = ratatui::layout::Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    frame.render_widget(ratatui::widgets::Clear, rect);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph, rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tree::{flatten_visible_rows, node_key_at_path},
        selection::{CachedModel, VendorKind, ranking::build_version_index},
        state::{
            Message, MessageKind, MessageSender, Node, NodeKind, NodeStatus, RunRecord, RunStatus,
            SessionState,
        },
        tmux::TmuxContext,
    };
    use ratatui::layout::Rect;
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };

    fn test_app(nodes: Vec<Node>, runs: Vec<RunRecord>, messages: Vec<Message>) -> App {
        let mut state = SessionState::new("render-test".to_string());
        state.agent_runs = runs;
        let selected_key = node_key_at_path(&nodes, &[0]);
        let visible_rows = flatten_visible_rows(&nodes, |row| row.is_expandable());
        let collapsed_overrides = visible_rows
            .iter()
            .filter(|row| row.is_expandable())
            .map(|row| (row.key.clone(), super::super::ExpansionOverride::Expanded))
            .collect();
        App {
            tmux: TmuxContext {
                session_name: "test".to_string(),
                window_index: "0".to_string(),
                window_name: "test".to_string(),
            },
            state,
            nodes,
            visible_rows,
            models: Vec::new(),
            versions: crate::selection::ranking::build_version_index(&[]),
            model_refresh: ModelRefreshState::Idle(Instant::now()),
            selected: 0,
            selected_key,
            collapsed_overrides,
            viewport_top: 0,
            follow_tail: true,
            explicit_viewport_scroll: false,
            tail_detach_baseline: None,
            body_inner_height: 20,
            body_inner_width: 80,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            pending_view_path: None,
            confirm_back: false,
            window_launched: false,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            agent_content_hash: 0,
            agent_last_change: None,
            spinner_tick: 0,
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_path: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            pending_drain_deadline: None,
            current_run_id: None,
            failed_models: HashMap::new(),
            test_launch_harness: None,
            messages,
        }
    }

    fn run_record(id: u64, status: RunStatus) -> RunRecord {
        RunRecord {
            id,
            stage: format!("run-{id}"),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "model".to_string(),
            vendor: "vendor".to_string(),
            window_name: format!("[Run {id}]"),
            started_at: chrono::Utc::now(),
            ended_at: if status == RunStatus::Running {
                None
            } else {
                Some(chrono::Utc::now())
            },
            status,
            error: None,
            effort: crate::adapters::EffortLevel::Normal,
            hostname: None,
            mount_device_id: None,
        }
    }

    fn message(run_id: u64, text: &str) -> Message {
        Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Summary,
            sender: MessageSender::Agent {
                model: "model".to_string(),
                vendor: "vendor".to_string(),
            },
            text: text.to_string(),
        }
    }

    fn model_with_axis_score(name: &str, axis_score: f64, display_order: usize) -> CachedModel {
        CachedModel {
            vendor: VendorKind::Codex,
            name: name.to_string(),
            overall_score: axis_score,
            current_score: 99.0,
            standard_error: 0.0,
            axes: vec![
                ("codequality".to_string(), axis_score),
                ("correctness".to_string(), axis_score),
                ("debugging".to_string(), axis_score),
                ("safety".to_string(), axis_score),
                ("complexity".to_string(), axis_score),
                ("edgecases".to_string(), axis_score),
                ("contextawareness".to_string(), axis_score),
                ("taskcompletion".to_string(), axis_score),
                ("stability".to_string(), axis_score),
            ],
            axis_provenance: std::collections::BTreeMap::new(),
            quota_percent: Some(100),
            display_order,
            fallback_from: None,
        }
    }

    fn vendor_model_with_axis_score(
        vendor: VendorKind,
        name: &str,
        axis_score: f64,
        display_order: usize,
    ) -> CachedModel {
        let mut model = model_with_axis_score(name, axis_score, display_order);
        model.vendor = vendor;
        model
    }

    fn full_buffer_line_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "))
            .collect()
    }

    fn spans_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn model_strip_bolds_only_phase_rank_one_when_percentages_round_together() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.set_models(vec![
            model_with_axis_score("gpt-alpha", 1.0, 0),
            model_with_axis_score("gpt-beta", 0.996_655, 1),
        ]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 90, 6);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let beta_y = (0..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("beta"))
            .expect("beta row should be rendered");
        let beta_line = full_buffer_line_text(&buf, beta_y);
        let build_col = beta_line
            .rfind("B50")
            .expect("beta build probability should round to B50") as u16;
        let build_cell = buf.cell((build_col, beta_y)).expect("build cell");

        assert!(!build_cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn model_strip_truncates_long_names_on_narrow_width() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.set_models(vec![model_with_axis_score(
            "gpt-very-long-model-name-that-will-overflow",
            1.0,
            0,
        )]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 50, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let model_y = (1..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("codex"))
            .expect("model row should be rendered");
        let model_line = full_buffer_line_text(&buf, model_y);

        assert!(
            model_line.contains("..."),
            "narrow width should truncate name with ellipsis: {model_line}"
        );
        assert!(
            !model_line.contains("very-long-model-name-that-will-overflow"),
            "full name should not fit: {model_line}"
        );
    }

    #[test]
    fn model_strip_preserves_metrics_on_narrow_width() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.set_models(vec![model_with_axis_score(
            "gpt-very-long-model-name-that-will-overflow",
            1.0,
            0,
        )]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 50, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let model_y = (1..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("codex"))
            .expect("model row should be rendered");
        let model_line = full_buffer_line_text(&buf, model_y);

        assert!(
            model_line.contains("I")
                && model_line.contains("P")
                && model_line.contains("B")
                && model_line.contains("R"),
            "metrics IPBR should still appear on narrow width: {model_line}"
        );
    }

    #[test]
    fn model_strip_truncates_fallback_marker_text_on_narrow_width() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        let mut model = model_with_axis_score("gpt-opus-4-1", 1.0, 0);
        model.fallback_from = Some("gpt-4-1".to_string());
        app.set_models(vec![model]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 48, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let model_y = (1..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("codex"))
            .expect("model row should be rendered");
        let model_line = full_buffer_line_text(&buf, model_y);

        assert!(
            model_line.contains("opus-4-1 (..."),
            "fallback marker should participate in truncation: {model_line}"
        );
        assert!(
            !model_line.contains("opus-4-1..."),
            "fallback marker should not be dropped before truncation: {model_line}"
        );
    }

    #[test]
    fn model_strip_shows_full_name_on_wide_width() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.set_models(vec![model_with_axis_score("gpt-opus-4-5-20251101", 1.0, 0)]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 90, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let model_y = (1..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("codex"))
            .expect("model row should be rendered");
        let model_line = full_buffer_line_text(&buf, model_y);

        assert!(
            model_line.contains("opus-4-5-20251101"),
            "full short name should appear on wide width: {model_line}"
        );
        assert!(
            !model_line.contains("..."),
            "should not truncate on wide width: {model_line}"
        );
    }

    #[test]
    fn model_strip_uses_gemini_preview_display_label() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.set_models(vec![vendor_model_with_axis_score(
            VendorKind::Gemini,
            "gemini-3.1-pro-preview",
            1.0,
            0,
        )]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 90, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let model_y = (1..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("gemini"))
            .expect("model row should be rendered");
        let model_line = full_buffer_line_text(&buf, model_y);

        assert!(
            model_line.contains("3.1-pro"),
            "short display label should appear: {model_line}"
        );
        assert!(
            !model_line.contains("3.1-pro-preview"),
            "preview suffix should not appear in display label: {model_line}"
        );
    }

    #[test]
    fn model_strip_shows_new_suffix_for_fallback_models_on_wide_width() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        let mut model = model_with_axis_score("gpt-opus-4-5-20251101", 1.0, 0);
        model.fallback_from = Some("gpt-4-5".to_string());
        app.set_models(vec![model]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 90, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let model_y = (1..area.height)
            .find(|y| full_buffer_line_text(&buf, *y).contains("codex"))
            .expect("model row should be rendered");
        let model_line = full_buffer_line_text(&buf, model_y);

        assert!(
            model_line.contains("opus-4-5-20251101 (new)"),
            "fallback model should show (new) suffix on wide width: {model_line}"
        );
    }

    #[test]
    fn model_strip_height_uses_only_visible_models() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.set_models(vec![
            model_with_axis_score("gpt-alpha", 100.0, 0),
            model_with_axis_score("gpt-beta", 95.0, 1),
            model_with_axis_score("gpt-gamma", 90.0, 2),
            model_with_axis_score("gpt-delta", 85.0, 3),
        ]);
        app.versions = build_version_index(&app.models);

        assert_eq!(model_strip_height(&app.models, &app.versions), 5);
    }

    #[test]
    fn model_strip_height_is_border_only_when_no_models_are_visible() {
        let app = test_app(Vec::new(), Vec::new(), Vec::new());

        assert_eq!(model_strip_height(&app.models, &app.versions), 2);
    }

    #[test]
    fn format_model_name_spans_exact_width() {
        // Full name fits — padded to target width.
        let spans = format_model_name_spans("short", false, 10);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 10, "should pad when name fits");

        // Name + suffix fits — padded to target width.
        let spans = format_model_name_spans("short", true, 15);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 15, "should pad when name + suffix fits");
        assert_eq!(spans_text(&spans), "short (new)    ");

        // Name truncated with ellipsis.
        let spans = format_model_name_spans("verylongname", false, 10);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 10, "truncated name should be exact width");
        assert!(spans.iter().any(|s| s.content.contains("...")));
        assert_eq!(spans_text(&spans), "verylon...");

        // Name fits but suffix doesn't — suffix participates in truncation.
        let spans = format_model_name_spans("gpt-4-turbo", true, 15);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 15, "partial suffix + ellipsis should be exact width");
        assert!(spans.iter().any(|s| s.content.contains("...")));
        assert_eq!(spans_text(&spans), "gpt-4-turbo ...");

        // Very narrow — only ellipsis fits.
        let spans = format_model_name_spans("x", false, 2);
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 2, "ultra-narrow should still be exact width");
    }

    fn node(
        label: &str,
        kind: NodeKind,
        status: NodeStatus,
        children: Vec<Node>,
        run_id: Option<u64>,
        leaf_run_id: Option<u64>,
    ) -> Node {
        Node {
            label: label.to_string(),
            kind,
            status,
            summary: format!("{label} summary"),
            children,
            run_id,
            leaf_run_id,
        }
    }

    fn nested_transcript_tree() -> Vec<Node> {
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Task A",
                NodeKind::Task,
                NodeStatus::Running,
                vec![node(
                    "Coder",
                    NodeKind::Mode,
                    NodeStatus::Running,
                    Vec::new(),
                    Some(1),
                    None,
                )],
                None,
                None,
            )],
            None,
            None,
        )]
    }

    fn line_text(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn render_lines(app: &App, height: u16) -> Vec<String> {
        let width = 80;
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        PipelineWidget { app }.render(area, &mut buf);
        (0..height).map(|y| line_text(&buf, y, width)).collect()
    }

    #[test]
    fn renders_depth_indented_visible_rows_and_inline_transcript() {
        let app = test_app(
            nested_transcript_tree(),
            vec![run_record(1, RunStatus::Running)],
            vec![message(1, "coder transcript body")],
        );

        let lines = render_lines(&app, 10);

        assert!(lines.iter().any(|line| line.contains("▾ Root | running")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains(" ▾ Task A | running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("  ▾ Coder | running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("coder transcript body"))
        );
    }

    #[test]
    fn expanded_structural_parents_do_not_render_duplicate_child_list_body() {
        let app = test_app(
            nested_transcript_tree(),
            vec![run_record(1, RunStatus::Running)],
            vec![message(1, "only the transcript body")],
        );

        let lines = render_lines(&app, 12);

        assert!(!lines.iter().any(|line| line.contains("── Task A")));
        assert!(!lines.iter().any(|line| line.contains("── Coder")));
    }

    #[test]
    fn collapsed_absorbed_simple_stage_renders_direct_transcript() {
        let app = test_app(
            vec![node(
                "Brainstorm",
                NodeKind::Stage,
                NodeStatus::Done,
                Vec::new(),
                None,
                Some(7),
            )],
            vec![run_record(7, RunStatus::Done)],
            vec![message(7, "absorbed transcript body")],
        );

        let lines = render_lines(&app, 8);

        assert!(lines.iter().any(|line| line.contains("Brainstorm | done")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("absorbed transcript body"))
        );
    }

    #[test]
    fn multiple_open_transcripts_share_body_height_in_visible_order() {
        let nodes = vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![
                node(
                    "First",
                    NodeKind::Mode,
                    NodeStatus::Running,
                    Vec::new(),
                    Some(1),
                    None,
                ),
                node(
                    "Second",
                    NodeKind::Mode,
                    NodeStatus::Running,
                    Vec::new(),
                    Some(2),
                    None,
                ),
            ],
            None,
            None,
        )];
        let app = test_app(
            nodes,
            vec![
                run_record(1, RunStatus::Running),
                run_record(2, RunStatus::Running),
            ],
            vec![
                message(1, "first transcript"),
                message(2, "second transcript"),
            ],
        );

        let lines = render_lines(&app, 9);
        let first_body = lines
            .iter()
            .position(|line| line.contains("first transcript"))
            .expect("first body rendered");
        let second_header = lines
            .iter()
            .position(|line| line.contains("Second | running"))
            .expect("second header rendered");
        let second_body = lines
            .iter()
            .position(|line| line.contains("second transcript"))
            .expect("second body rendered");

        assert!(first_body < second_header);
        assert!(second_header < second_body);
    }

    #[test]
    fn failed_unverified_render_shows_distinct_status_and_stamp_hint() {
        let app = test_app(
            vec![node(
                "Coder",
                NodeKind::Mode,
                NodeStatus::FailedUnverified,
                Vec::new(),
                Some(1),
                None,
            )],
            vec![run_record(1, RunStatus::FailedUnverified)],
            vec![Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: MessageSender::System,
                text: "attempt 1 unverified: missing finish stamp at artifacts/run-finish/coder-t1-r1-a1.toml".to_string(),
            }],
        );

        let lines = render_lines(&app, 8);

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Coder | failed-unverified"))
        );
        assert!(lines.iter().any(|line| line.contains("run-finish")));
    }

    #[test]
    fn header_only_viewports_render_headers_without_body() {
        let app = test_app(
            nested_transcript_tree(),
            vec![run_record(1, RunStatus::Running)],
            vec![message(1, "hidden transcript body")],
        );

        let lines = render_lines(&app, 5);

        assert!(lines.iter().any(|line| line.contains("Root | running")));
        assert!(lines.iter().any(|line| line.contains("Task A | running")));
        assert!(lines.iter().any(|line| line.contains("Coder | running")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("hidden transcript body"))
        );
    }

    fn tall_app() -> App {
        let nodes = nested_transcript_tree();
        let mut messages = Vec::new();
        for i in 0..50 {
            messages.push(message(1, &format!("message {i}")));
        }
        let runs = vec![run_record(1, RunStatus::Running)];
        let mut app = test_app(nodes, runs, messages);
        app.body_inner_height = 5;
        app.body_inner_width = 80;
        app
    }

    fn transcript_then_stage_tree() -> Vec<Node> {
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![
                node(
                    "Coder",
                    NodeKind::Mode,
                    NodeStatus::Running,
                    Vec::new(),
                    Some(1),
                    None,
                ),
                node(
                    "Review",
                    NodeKind::Stage,
                    NodeStatus::Pending,
                    Vec::new(),
                    None,
                    None,
                ),
            ],
            None,
            None,
        )]
    }

    #[test]
    fn explicit_page_scroll_moves_viewport_without_focus_clamping() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.selected = 0;
        let step = app.body_inner_height.saturating_sub(1).max(1) as isize;
        app.scroll_viewport(step, true);
        assert_eq!(app.selected, 0);
        assert!(app.explicit_viewport_scroll);
        app.clamp_viewport();
        assert_eq!(app.selected, 0);
        assert!(app.viewport_top > 0);
    }

    #[test]
    fn page_scroll_to_bottom_reattaches_tail_and_hides_badge() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.messages.push(message(1, "new unread"));
        let max_top = app.max_viewport_top();
        app.scroll_viewport(max_top as isize, true);
        app.clamp_viewport();
        assert!(app.follow_tail);
        assert_eq!(app.tail_detach_baseline, None);
        let lines = render_lines(&app, app.body_inner_height as u16 + 2);
        assert!(!lines.iter().any(|l| l.contains("↓")));
    }

    #[test]
    fn unread_badge_shows_when_new_content_below_viewport() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.messages.push(message(1, "new unread"));
        app.viewport_top = 0;
        app.clamp_viewport();
        let lines = render_lines(&app, app.body_inner_height as u16 + 2);
        assert!(lines.iter().any(|l| l.contains("↓ 1 new")));
    }

    #[test]
    fn unread_badge_hides_once_first_unread_line_is_visible() {
        let mut app = test_app(
            transcript_then_stage_tree(),
            vec![run_record(1, RunStatus::Running)],
            vec![
                message(1, "old message 1"),
                message(1, "old message 2"),
                message(1, "old message 3"),
            ],
        );
        app.body_inner_height = 5;
        app.body_inner_width = 80;
        app.set_follow_tail(false);
        app.messages.push(message(1, "new unread"));
        app.scroll_viewport(2, true);

        let lines = render_lines(&app, app.body_inner_height as u16 + 2);

        assert!(lines.iter().any(|line| line.contains("new unread")));
        assert!(!lines.iter().any(|line| line.contains("↓")));
    }

    #[test]
    fn page_up_scrolls_viewport_without_moving_focus() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.viewport_top = app.max_viewport_top();
        app.selected = 2;
        let initial_selected = app.selected;
        let step = app.body_inner_height.saturating_sub(1).max(1) as isize;
        app.scroll_viewport(-step, true);
        assert_eq!(app.selected, initial_selected);
        assert!(app.viewport_top < app.max_viewport_top());
        app.clamp_viewport();
        assert_eq!(app.selected, initial_selected);
    }

    #[test]
    fn page_down_key_pages_without_moving_focus() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        let initial_key = app.selected_key.clone();
        let step = app.body_inner_height.saturating_sub(1).max(1);

        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_key, initial_key);
        assert_eq!(app.viewport_top, step);
        assert!(app.explicit_viewport_scroll);
    }

    #[test]
    fn page_up_key_pages_without_moving_focus() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.viewport_top = app.max_viewport_top();
        app.explicit_viewport_scroll = true;
        app.selected = 2;
        app.selected_key = Some(app.visible_rows[2].key.clone());
        let initial_key = app.selected_key.clone();
        let initial_top = app.viewport_top;
        let step = app.body_inner_height.saturating_sub(1).max(1);

        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(app.selected, 2);
        assert_eq!(app.selected_key, initial_key);
        assert_eq!(app.viewport_top, initial_top.saturating_sub(step));
        assert!(app.explicit_viewport_scroll);
    }

    #[test]
    fn focus_driven_scroll_clears_explicit_flag() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.scroll_viewport(5, true);
        assert!(app.explicit_viewport_scroll);
        app.scroll_viewport(1, false);
        assert!(!app.explicit_viewport_scroll);
    }

    #[test]
    fn clamp_viewport_restores_focus_visibility_after_focus_movement() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.viewport_top = 10;
        app.selected = 0;
        app.explicit_viewport_scroll = false;
        app.clamp_viewport();
        let (ys, _) = app.header_y_offsets();
        let section_bottom = ys.get(1).copied().unwrap_or(ys.len());
        assert!(app.viewport_top < section_bottom);
    }

    #[test]
    fn clamp_viewport_reattaches_tail_when_bottom_shrinks_under_viewport() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.messages.push(message(1, "new unread"));
        app.viewport_top = 10;
        app.body_inner_height = 200;

        app.clamp_viewport();

        assert!(app.follow_tail);
        assert_eq!(app.tail_detach_baseline, None);
        assert_eq!(app.viewport_top, app.max_viewport_top());
    }

    #[test]
    fn render_spec_review_paused_modal_without_panic() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::SpecReviewPaused;
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn render_plan_review_paused_modal_without_panic() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::PlanReviewPaused;
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn render_stage_error_modal_without_panic() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::SpecReviewRunning;
        app.state.agent_error = Some("model timeout".to_string());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn model_strip_renders_provenance_labels_verbatim() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        let mut model = model_with_axis_score("gpt-alpha", 1.0, 0);
        model.axis_provenance = std::collections::BTreeMap::from([
            ("correctness".to_string(), "suite:hourly".to_string()),
            ("debugging".to_string(), "suite:deep".to_string()),
            (
                "contextwindow".to_string(),
                "dropped:contextwindow".to_string(),
            ),
        ]);
        app.set_models(vec![model]);
        app.versions = build_version_index(&app.models);

        let area = Rect::new(0, 0, 120, 6);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.model_strip(area.width).render(area, &mut buf);

        let all_text: String = (0..area.height)
            .map(|y| full_buffer_line_text(&buf, y))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            all_text.contains("correctness: suite:hourly"),
            "should display correctness provenance verbatim: {all_text}"
        );
        assert!(
            all_text.contains("debugging: suite:deep"),
            "should display debugging provenance verbatim: {all_text}"
        );
        assert!(
            all_text.contains("contextwindow: dropped:contextwindow"),
            "should display contextwindow drop label verbatim: {all_text}"
        );
    }

    #[test]
    fn model_strip_height_accounts_for_provenance_lines() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        let mut model = model_with_axis_score("gpt-alpha", 1.0, 0);
        model.axis_provenance = std::collections::BTreeMap::from([(
            "correctness".to_string(),
            "suite:hourly".to_string(),
        )]);
        app.set_models(vec![model]);
        app.versions = build_version_index(&app.models);

        // 1 model × 2 lines (model + provenance) + 2 borders = 4
        assert_eq!(model_strip_height(&app.models, &app.versions), 4);
    }
}
