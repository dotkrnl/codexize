use ratatui::{
    Frame,
    buffer::Buffer,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget},
};

use crate::state::{NodeStatus, Phase, RunRecord, RunStatus};
use chrono::Offset;

#[cfg(test)]
use super::state::ModelRefreshState;
use super::{
    App, ModalKind, StageId, chat_widget,
    chrome::{UnreadBadge, bottom_rule, top_rule_with_left_spans},
    clock::{Clock, WallClock},
    focus_caps::FocusCaps,
    footer::{
        CachedSummaryFetcher, TranscriptLeafMarker, extract_short_title,
        format_running_transcript_leaf, keymap,
    },
    models_area,
    sheet::bottom_sheet,
};
use crate::tui::wrap_input;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const DEGENERATE_FLOOR: u16 = 16;
const BODY_FLOOR_NORMAL: u16 = 8;

struct PipelineWidget<'a> {
    app: &'a App,
}

impl Widget for PipelineWidget<'_> {
    fn render(self, area: ratatui::layout::Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

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

        let area_h = area.height as usize;
        let viewport_top = self
            .app
            .viewport_top
            .min(self.app.max_viewport_top_for_height(area_h));
        let pinned_header = self.app.pinned_running_header(viewport_top);
        let (content_y, content_h) = if let Some((index, _)) = pinned_header {
            if let Some(node) = self.app.node_for_row(index) {
                let expanded = self.app.is_expanded(index);
                let line = self.app.node_header(index, expanded, node);
                buf.set_line(area.x, area.y, &line, area.width);
            }
            // Pinning only happens when the header's natural y is above the
            // viewport, so the source slice cannot also contain it.
            (area.y.saturating_add(1), area_h.saturating_sub(1))
        } else {
            (area.y, area_h)
        };

        let end = (viewport_top + content_h).min(lines.len());
        for (offset, line) in lines[viewport_top..end].iter().enumerate() {
            buf.set_line(area.x, content_y + offset as u16, line, area.width);
        }
    }
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

pub fn sanitize_live_summary(text: &str) -> String {
    let stripped = strip_ansi_codes(text);
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(500).collect()
}

/// Determines whether the row at `index` is the last sibling at its depth.
///
/// Scans forward from `index+1` until a row with depth <= current depth is found.
/// Returns true if no such row exists or if the found row has depth < current depth.
fn is_last_sibling(visible_rows: &[super::tree::VisibleNodeRow], index: usize) -> bool {
    let cur_depth = visible_rows[index].depth;
    visible_rows[index + 1..]
        .iter()
        .find(|r| r.depth <= cur_depth)
        .map(|r| r.depth < cur_depth)
        .unwrap_or(true)
}

impl App {
    pub(super) fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let term_h = area.height;
        let width = area.width;
        let degenerate = term_h < DEGENERATE_FLOOR;

        // --- Models area (top) ---
        let (model_lines, models_mode) = if degenerate {
            (Vec::new(), self.prev_models_mode)
        } else {
            models_area::responsive_models_area(
                &self.models,
                &self.versions,
                &self.quota_errors,
                width,
                term_h,
                self.prev_models_mode,
            )
        };
        self.prev_models_mode = models_mode;
        let models_h = model_lines.len() as u16;

        // --- Status line (tick + render) ---
        let now = std::time::Instant::now();
        self.status_line.borrow_mut().tick(now);
        let status_line_content = if degenerate {
            None
        } else {
            self.status_line.borrow().render()
        };
        let status_h: u16 = if status_line_content.is_some() { 1 } else { 0 };

        // --- Determine footer zone ---
        // Three-way branch: modal → full-screen overlay (drawn over the body
        // after the bottom rule); input mode → bottom sheet; otherwise →
        // status + keymap. The modal arm is an overlay, not a footer sheet,
        // so it does not consume footer rows.
        let modal = self.active_modal();

        let caps = self.focus_caps();
        let keymap_line = keymap(
            self.state.current_phase,
            modal,
            caps,
            self.input_mode,
            width,
        );

        // Sheet content is owned by the input-mode path only. Modal content
        // is computed independently inside the overlay branch below.
        let sheet_content: Option<Vec<Line<'static>>> = if self.input_mode {
            Some(self.input_sheet_content(width))
        } else {
            None
        };

        // Footer height: only the input-mode sheet (when active) plus the
        // always-present keymap+status lines contribute. Modal state is
        // overlaid and does not change body height.
        let footer_h = if let Some(ref content) = sheet_content {
            // Sheet = rule + content + controls. Min: rule + controls = 2.
            let desired = (content.len() as u16).saturating_add(2);
            let max_for_sheet = if degenerate {
                // Degenerate: sheet wins over body entirely.
                term_h.saturating_sub(models_h).saturating_sub(2) // top + bottom rule
            } else {
                term_h
                    .saturating_sub(models_h)
                    .saturating_sub(2) // rules
                    .saturating_sub(BODY_FLOOR_NORMAL)
            };
            desired.min(max_for_sheet).max(1)
        } else {
            1 + status_h // keymap + optional status
        };

        // --- Body height ---
        let chrome_h = models_h + 1 + 1 + footer_h; // models + top rule + bottom rule + footer
        let body_h = term_h.saturating_sub(chrome_h);

        self.body_inner_height = body_h as usize;
        self.body_inner_width = width as usize;
        self.latch_visible_expansions();
        self.clamp_viewport();

        // --- Render top-down ---
        let mut y = area.y;

        // 1. Models area
        if models_h > 0 {
            let models_area = ratatui::layout::Rect::new(area.x, y, width, models_h);
            frame.render_widget(Paragraph::new(model_lines), models_area);
            y += models_h;
        }

        // 2. Top rule
        let top_rule_line = self.build_top_rule(width);
        let top_rule_area = ratatui::layout::Rect::new(area.x, y, width, 1);
        frame.render_widget(Paragraph::new(vec![top_rule_line]), top_rule_area);
        y += 1;

        // 3. Pipeline body
        if body_h > 0 {
            let body_area = ratatui::layout::Rect::new(area.x, y, width, body_h);
            frame.render_widget(PipelineWidget { app: self }, body_area);
            y += body_h;
        }

        // 4. Bottom rule (with unread badge)
        let badge = self.unread_badge();
        let bottom_rule_line = bottom_rule(width, badge);
        let bottom_rule_area = ratatui::layout::Rect::new(area.x, y, width, 1);
        frame.render_widget(Paragraph::new(vec![bottom_rule_line]), bottom_rule_area);
        y += 1;

        // 5. Footer zone — three-way branch (see "Determine footer zone").
        if let Some(m) = modal {
            let terminal_width = area.width;
            let terminal_height = area.height;
            let max_w = terminal_width.saturating_sub(4).max(1);
            let dialog_w = max_w.min(80).max(max_w.min(40));
            let inner_w = dialog_w.saturating_sub(2);
            let content = self.modal_content_lines(m, inner_w);
            let content_h = content.len();
            let dialog_h = ((content_h + 3) as u16).min(terminal_height.saturating_sub(2));
            let dialog = ratatui::layout::Rect::new(
                (terminal_width.saturating_sub(dialog_w)) / 2,
                (terminal_height.saturating_sub(dialog_h)) / 2,
                dialog_w,
                dialog_h,
            );

            // Clear only the dialog rect so the surrounding top/body chrome
            // remains visible behind the centered overlay.
            frame.render_widget(Clear, dialog);
            let block = Block::bordered().border_style(Style::default().fg(Color::DarkGray));
            frame.render_widget(block.clone(), dialog);
            let inner = block.inner(dialog);

            if inner.height > 0 && inner.width > 0 {
                let modal_keymap =
                    keymap(self.state.current_phase, Some(m), caps, false, inner.width);
                let inner_h = inner.height as usize;

                // The keymap occupies the last inner row, so content can use
                // up to `inner_h - 1` rows. If content overflows, truncate
                // and append a single `…` row in default style — the spec
                // requires the keymap to remain reachable.
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
                    &modal_keymap,
                    inner.width,
                );
            }
        } else if let Some(content) = sheet_content {
            let sheet_lines = bottom_sheet(content, keymap_line, footer_h, width);
            for line in sheet_lines {
                if y >= area.y + area.height {
                    break;
                }
                let line_area = ratatui::layout::Rect::new(area.x, y, width, 1);
                frame.render_widget(Paragraph::new(vec![line]), line_area);
                y += 1;
            }
        } else {
            // Status line (optional)
            if let Some(status) = status_line_content
                && y < area.y + area.height
            {
                let status_area = ratatui::layout::Rect::new(area.x, y, width, 1);
                frame.render_widget(Paragraph::new(vec![status]), status_area);
                y += 1;
            }
            // Keymap (always last)
            if y < area.y + area.height {
                let keymap_area = ratatui::layout::Rect::new(area.x, y, width, 1);
                frame.render_widget(Paragraph::new(vec![keymap_line]), keymap_area);
            }
        }

        if self.palette.open && area.height > 0 && area.width > 0 {
            let overlay_h = area.height.min(2);
            let overlay = ratatui::layout::Rect::new(
                area.x,
                area.y + area.height.saturating_sub(overlay_h),
                area.width,
                overlay_h,
            );
            frame.render_widget(Clear, overlay);
            let lines = self.palette_overlay_lines(width);
            let visible = lines
                .into_iter()
                .take(overlay_h as usize)
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(visible), overlay);
        }
    }

    fn palette_overlay_lines(&self, width: u16) -> Vec<Line<'static>> {
        let commands = self.palette_commands();
        let buffer = self.palette.buffer.clone();
        let ghost = super::palette::ghost_completion(&buffer, &commands)
            .filter(|candidate| !candidate.is_empty())
            .unwrap_or("");
        let suffix = ghost.strip_prefix(buffer.trim()).unwrap_or("");
        let mut input_spans = vec![
            Span::styled(
                ":",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(buffer),
        ];
        if !suffix.is_empty() {
            input_spans.push(Span::styled(
                suffix.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let mut help = "Esc close  Tab complete  Enter run".to_string();
        if width < help.chars().count() as u16 {
            help.truncate(width as usize);
        }
        vec![
            Line::from(input_spans),
            Line::from(Span::styled(help, Style::default().fg(Color::DarkGray))),
        ]
    }

    fn focus_caps(&self) -> FocusCaps {
        FocusCaps {
            can_expand: self
                .visible_rows
                .get(self.selected)
                .is_some_and(|row| row.is_expandable()),
            can_edit: self.editable_artifact().is_some(),
            can_back: self.can_go_back(),
            can_input: self.can_focus_input(),
        }
    }

    fn build_top_rule(&self, width: u16) -> Line<'static> {
        let project = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        let right = self.top_rule_right_text();
        top_rule_with_left_spans(self.top_rule_left_spans(&project), right.as_deref(), width)
    }

    fn top_rule_left_spans(&self, project: &str) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(
            project.to_string(),
            Style::default().fg(Color::DarkGray),
        )];
        if self.state.modes.yolo {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                "[YOLO]".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }
        if self.state.modes.cheap {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                "[CHEAP]".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans
    }

    fn top_rule_right_text(&self) -> Option<String> {
        // When a run is active, show "<agent-name> · <live-summary-title>".
        // Otherwise show "<stage-label> · <state-label>".
        if let Some(run_id) = self.current_run_id
            && let Some(run) = self.state.agent_runs.iter().find(|r| r.id == run_id)
        {
            let agent = &run.window_name;
            let summary = if self.live_summary_cached_text.is_empty() {
                self.state.current_phase.label()
            } else {
                extract_short_title(&self.live_summary_cached_text)
            };
            return Some(format!("{} · {}", agent, summary));
        }

        let label = self.state.current_phase.label();
        let state_label = self.phase_state_label();
        Some(format!("{} · {}", label, state_label))
    }

    fn phase_state_label(&self) -> &'static str {
        if self.state.agent_error.is_some() {
            return "error";
        }
        match self.state.current_phase {
            Phase::IdeaInput | Phase::BlockedNeedsUser => "awaiting input",
            Phase::SpecReviewPaused | Phase::PlanReviewPaused => "paused",
            Phase::SkipToImplPending | Phase::GitGuardPending => "awaiting input",
            Phase::Done => "done",
            _ => "running",
        }
    }

    fn unread_badge(&self) -> Option<UnreadBadge> {
        let unread = self.unread_below_count();
        let at_bottom = self.viewport_top >= self.max_viewport_top();
        let viewport_bottom = self.viewport_top + self.effective_body_inner_height();
        let unread_below_viewport = self
            .first_unread_rendered_line()
            .map(|line| line >= viewport_bottom)
            .unwrap_or(!at_bottom);

        if unread > 0 && unread_below_viewport {
            Some(UnreadBadge { count: unread })
        } else {
            None
        }
    }

    fn modal_content_lines(&self, modal: ModalKind, width: u16) -> Vec<Line<'static>> {
        match modal {
            ModalKind::SkipToImpl => skip_to_impl_content(
                self.state.skip_to_impl_rationale.as_deref(),
                self.state.skip_to_impl_kind,
                width,
            ),
            ModalKind::GitGuard => guard_content(self.state.pending_guard_decision.as_ref()),
            ModalKind::SpecReviewPaused => vec![Line::from(Span::styled(
                "Spec review complete".to_string(),
                Style::default().fg(Color::White),
            ))],
            ModalKind::PlanReviewPaused => vec![Line::from(Span::styled(
                "Plan review complete".to_string(),
                Style::default().fg(Color::White),
            ))],
            ModalKind::StageError(stage_id) => {
                stage_error_content(stage_id, self.state.agent_error.as_deref(), width)
            }
        }
    }

    fn input_sheet_content(&self, width: u16) -> Vec<Line<'static>> {
        let inner_width = (width as usize).saturating_sub(4).max(1);
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
        let is_focused = index == self.selected;
        let depth = self.visible_rows[index].depth;

        // Structural focus marker: `▌` in the gutter for the selected row.
        let focus_glyph = if is_focused { "▌" } else { " " };

        // Thin tree glyphs for indentation.
        let indent = if depth > 0 {
            let connector = if is_last_sibling(&self.visible_rows, index) {
                "└─"
            } else {
                "├─"
            };
            format!("{}{}", "│ ".repeat(depth.saturating_sub(1)), connector)
        } else {
            String::new()
        };

        let mut style = if is_focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        if node.status == NodeStatus::Pending {
            style = style.fg(Color::DarkGray);
        }

        let underline_active = matches!(
            node.status,
            NodeStatus::Running
                | NodeStatus::Done
                | NodeStatus::Failed
                | NodeStatus::FailedUnverified,
        );

        // Depth 0: underline the line style so `Buffer::set_line` extends the
        // underline across the trailing fill, producing a full-width rule.
        if depth == 0 {
            style = style.add_modifier(Modifier::UNDERLINED);
        }

        let dim = Style::default().fg(Color::DarkGray);

        let mut spans = vec![
            Span::styled(focus_glyph, Style::default()),
            Span::styled(indent, dim),
            Span::raw(format!("{} ", marker)),
            Span::raw(node.label.clone()),
            Span::styled(" · ", dim),
            Span::styled(node.status.label(), node.status.style()),
        ];
        if node.label == "Loop" && !node.summary.is_empty() {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(node.summary.clone(), dim));
        }

        // Depth 1+: underline only the text-bearing spans so the focus glyph
        // and tree indent stay un-underlined. Spans 0 (focus glyph) and 1
        // (indent) are skipped; everything else carries the modifier when the
        // status calls for it.
        if depth > 0 && underline_active {
            for span in &mut spans[2..] {
                span.style = span.style.add_modifier(Modifier::UNDERLINED);
            }
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
            let running_tail = self.running_tail_for_row(index, run, &WallClock::new());
            return chat_widget::message_lines(
                &msgs,
                run,
                local_offset,
                running_tail,
                available_width,
            );
        }
        self.render_compact_node(node, index)
    }

    /// Choose the trailing line that closes a still-running transcript body.
    ///
    /// Per spec, leaf transcript rows render the tail as a "live agent
    /// message" (`HH:MM:SS ⠋ live-summary-title`). Container rows whose
    /// children list visibly extends below them keep the legacy tree-shape
    /// spinner with a state label so the tree topology is preserved.
    fn running_tail_for_row<C: Clock>(
        &self,
        index: usize,
        run: &RunRecord,
        clock: &C,
    ) -> Option<Line<'static>> {
        if run.status != RunStatus::Running {
            return None;
        }
        let row = self.visible_rows.get(index)?;
        if row.has_children {
            let spin = spinner_frame(self.spinner_tick);
            let dim = Style::default().fg(Color::DarkGray);
            let gutter = "│ ".repeat(row.depth);
            return Some(Line::from(vec![
                Span::styled(format!(" {gutter}  "), dim),
                Span::styled(
                    spin,
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  running".to_string(), dim),
            ]));
        }
        let phase_label = self.state.current_phase.label();
        let fetcher = CachedSummaryFetcher::new(&self.live_summary_cached_text, &phase_label);
        Some(format_running_transcript_leaf(
            TranscriptLeafMarker::new(),
            clock,
            self.spinner_tick,
            &fetcher,
        ))
    }

    fn render_compact_node(&self, node: &crate::state::Node, index: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let depth = self.visible_rows.get(index).map_or(0, |r| r.depth);
        let gutter = "│ ".repeat(depth);
        let dim = Style::default().fg(Color::DarkGray);

        if node.status == NodeStatus::Running && self.window_launched {
            let spin = spinner_frame(self.spinner_tick);
            lines.push(Line::from(vec![
                Span::styled(format!(" {gutter}  "), dim),
                Span::styled(
                    spin,
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  running · {} lines", self.agent_line_count), dim),
            ]));
        }
        if !node.children.is_empty() {
            lines.push(Line::from(""));
            let child_count = node.children.len();
            for (i, child) in node.children.iter().enumerate() {
                let is_last = i == child_count - 1;
                let branch = if is_last { "└─" } else { "├─" };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {gutter}  {branch} "), dim),
                    Span::styled(
                        format!("{} ", child.label),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({})", child.status.label()), child.status.style()),
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

fn skip_to_impl_content(
    rationale: Option<&str>,
    kind: Option<crate::artifacts::SkipToImplKind>,
    width: u16,
) -> Vec<Line<'static>> {
    use crate::artifacts::SkipToImplKind;

    let is_nothing = kind == Some(SkipToImplKind::NothingToDo);
    let header = if is_nothing {
        "The brainstorm agent found nothing to implement."
    } else {
        "The brainstorm agent proposes skipping directly to implementation."
    };

    let rationale_text = rationale
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no rationale provided)");
    let rationale_lines = wrap_input(rationale_text, width.max(1) as usize);

    let mut lines = vec![
        Line::from(Span::styled(
            header.to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Rationale: ".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    for line in rationale_lines {
        lines.push(Line::from(line));
    }

    lines
}

fn guard_content(decision: Option<&crate::state::PendingGuardDecision>) -> Vec<Line<'static>> {
    let (captured_short, current_short) = decision
        .map(|d| {
            let cap = d.captured_head.get(..7).unwrap_or(&d.captured_head);
            let cur = d.current_head.get(..7).unwrap_or(&d.current_head);
            (cap.to_string(), cur.to_string())
        })
        .unwrap_or_else(|| ("???????".to_string(), "???????".to_string()));

    vec![
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
    ]
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

fn stage_error_content(stage_id: StageId, error: Option<&str>, width: u16) -> Vec<Line<'static>> {
    let title = stage_error_title(stage_id);
    let error_text = error
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no error details)");
    let truncated: String = error_text.chars().take(300).collect();
    let wrapped = wrap_input(&truncated, width.max(1) as usize);

    let mut lines = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for line in wrapped {
        lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(Color::White),
        )));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tree::{flatten_visible_rows, node_key_at_path},
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
            pending_yolo_toggle_gate: None,
            yolo_exit_issued: std::collections::HashSet::new(),
            test_launch_harness: None,
            messages,
            status_line: std::rc::Rc::new(std::cell::RefCell::new(
                super::super::status_line::StatusLine::new(),
            )),
            prev_models_mode: super::super::models_area::ModelsAreaMode::default(),
            palette: super::super::palette::PaletteState::default(),
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
            modes: crate::state::LaunchModes::default(),
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

    // model_strip_* full-table rendering tests have moved to
    // src/app/models_area.rs and target the new responsive_models_area
    // entry point. The underlying model_strip / model_strip_height /
    // format_model_name_spans helpers stay alive here only until the
    // chrome cutover wires the new renderer into App::draw.

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
                    "Builder",
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

        assert!(
            lines
                .iter()
                .any(|line| line.contains("▾ Root") && line.contains("running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("└─▾ Task A") && line.contains("running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("└─▾ Builder") && line.contains("running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Coder transcript body"))
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
        assert!(!lines.iter().any(|line| line.contains("── Builder")));
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

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Brainstorm") && line.contains("done"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Absorbed transcript body"))
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
            .position(|line| line.contains("First transcript"))
            .expect("first body rendered");
        let second_header = lines
            .iter()
            .position(|line| line.contains("Second") && line.contains("running"))
            .expect("second header rendered");
        let second_body = lines
            .iter()
            .position(|line| line.contains("Second transcript"))
            .expect("second body rendered");

        assert!(first_body < second_header);
        assert!(second_header < second_body);
    }

    #[test]
    fn failed_unverified_render_shows_distinct_status_and_stamp_hint() {
        let app = test_app(
            vec![node(
                "Builder",
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
                .any(|line| line.contains("Builder") && line.contains("failed-unverified"))
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

        let lines = render_lines(&app, 3);

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Root") && line.contains("running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Task A") && line.contains("running"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Builder") && line.contains("running"))
        );
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
                    "Builder",
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
        assert!(
            app.unread_badge().is_none(),
            "badge should be hidden at bottom"
        );
    }

    #[test]
    fn unread_badge_shows_when_new_content_below_viewport() {
        let mut app = tall_app();
        app.set_follow_tail(false);
        app.messages.push(message(1, "new unread"));
        app.viewport_top = 0;
        app.clamp_viewport();
        let badge = app.unread_badge();
        assert!(badge.is_some(), "should report unread badge");
        assert_eq!(badge.unwrap().count, 1);
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
        assert!(lines.iter().any(|line| line.contains("New unread")));
        assert!(
            app.unread_badge().is_none(),
            "badge should be hidden when unread is visible"
        );
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

    fn leaf_only_tree() -> Vec<Node> {
        vec![node(
            "Brainstorm",
            NodeKind::Stage,
            NodeStatus::Running,
            Vec::new(),
            None,
            Some(7),
        )]
    }

    #[test]
    fn running_leaf_row_renders_live_agent_message_not_legacy_working() {
        let mut app = test_app(
            leaf_only_tree(),
            vec![run_record(7, RunStatus::Running)],
            vec![message(7, "earlier transcript line")],
        );
        app.live_summary_cached_text = "drafting plan | full body of work".to_string();
        app.state.current_phase = Phase::PlanningRunning;

        let lines = render_lines(&app, 12);

        assert!(
            !lines.iter().any(|l| l.contains("working...")),
            "running leaf must not emit the legacy 'working...' line"
        );
        assert!(
            lines.iter().any(|l| l.contains("Drafting plan")),
            "running leaf tail should surface the live-summary short title"
        );
        assert!(
            lines.iter().any(|l| l.contains("Earlier transcript line")),
            "historical messages must still render"
        );
    }

    #[test]
    fn running_leaf_falls_back_to_phase_label_when_no_live_summary() {
        let mut app = test_app(
            leaf_only_tree(),
            vec![run_record(7, RunStatus::Running)],
            Vec::new(),
        );
        app.state.current_phase = Phase::BrainstormRunning;

        let lines = render_lines(&app, 8);

        assert!(
            lines.iter().any(|l| l.contains("Brainstorming")),
            "leaf tail should fall back to phase label when live-summary is empty"
        );
        assert!(!lines.iter().any(|l| l.contains("working...")));
    }

    #[test]
    fn running_tail_omitted_when_run_completes() {
        let app = test_app(
            leaf_only_tree(),
            vec![run_record(7, RunStatus::Done)],
            vec![message(7, "final summary")],
        );

        let lines = render_lines(&app, 8);

        assert!(lines.iter().any(|l| l.contains("Final summary")));
        assert!(!lines.iter().any(|l| l.contains("working...")));
    }

    #[test]
    fn container_row_running_tail_keeps_tree_shape_spinner() {
        // Container with visible children: the root row's body (if any) keeps
        // the legacy tree-shape spinner while children render their own
        // live-agent-message tails.
        let nodes = vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            )],
            None,
            // Root absorbs Builder's run via leaf_run_id so its body renders
            // the transcript inline.
            Some(1),
        )];
        let app = test_app(
            nodes,
            vec![run_record(1, RunStatus::Running)],
            vec![message(1, "shared transcript")],
        );

        let row = &app.visible_rows[0];
        let run = &app.state.agent_runs[0];
        let clock = super::super::clock::WallClock::new();
        let tail = app
            .running_tail_for_row(0, run, &clock)
            .expect("running container should produce a tail line");

        let text: String = tail.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            row.has_children,
            "container precondition: has visible children"
        );
        assert!(
            text.contains("running"),
            "container tail should keep the 'running' state label"
        );
        assert!(
            !text.contains("working..."),
            "container tail must not regress to the legacy cyan 'working...' line"
        );
    }

    fn render_full_frame(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h).map(|y| line_text(&buf, y, w)).collect()
    }

    fn render_full_frame_buf(app: &mut App, w: u16, h: u16) -> Buffer {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn raw_line_text(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>()
    }

    fn expected_dialog_rect(terminal_width: u16, terminal_height: u16, content_h: usize) -> Rect {
        let max_w = terminal_width.saturating_sub(4).max(1);
        let dialog_w = max_w.min(80).max(max_w.min(40));
        let dialog_h = ((content_h + 3) as u16).min(terminal_height.saturating_sub(2));
        Rect::new(
            (terminal_width - dialog_w) / 2,
            (terminal_height - dialog_h) / 2,
            dialog_w,
            dialog_h,
        )
    }

    #[test]
    fn palette_overlay_renders_buffer_and_ghost_completion() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.palette.open();
        app.palette.buffer = "qu".to_string();
        app.palette.cursor = 2;

        let lines = render_full_frame(&mut app, 80, 24);
        let text = lines.join("\n");

        assert!(text.contains(":qu"));
        assert!(
            text.contains("quit"),
            "ghost completion should make the target command visible"
        );
    }

    fn impl_round_2_running_app() -> App {
        let nodes = vec![node(
            "Implementation",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(42),
                None,
            )],
            None,
            None,
        )];
        let mut app = test_app(
            nodes,
            vec![run_record(42, RunStatus::Running)],
            // No historical messages — the test only needs the live running
            // transcript leaf so the assertion surface is deterministic.
            Vec::new(),
        );
        app.state.current_phase = Phase::ImplementationRound(2);
        app.live_summary_cached_text =
            "wiring full-screen tests | adding render-level snapshot coverage".to_string();
        app.current_run_id = Some(42);
        app
    }

    /// Render at a width that fits the full default keymap (so `q quit`
    /// appears verbatim on the last line, anchoring the assertion).
    const FULL_FRAME_WIDTH: u16 = 200;

    /// Replace wall-clock timestamps with stable placeholders so that
    /// full-frame vector assertions are deterministic regardless of when or
    /// where the test runs.
    fn normalize_frame(lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                // Running transcript leaf: "HH:MM:SS ⠋ ..."
                if line.len() >= 9
                    && line.as_bytes()[0].is_ascii_digit()
                    && line.as_bytes()[1].is_ascii_digit()
                    && line.as_bytes()[2] == b':'
                    && line.as_bytes()[3].is_ascii_digit()
                    && line.as_bytes()[4].is_ascii_digit()
                    && line.as_bytes()[5] == b':'
                    && line.as_bytes()[6].is_ascii_digit()
                    && line.as_bytes()[7].is_ascii_digit()
                    && line.as_bytes()[8] == b' '
                {
                    format!("XX:XX:XX{}", &line[8..])
                } else {
                    line
                }
            })
            .collect()
    }

    #[test]
    fn full_screen_idea_input_renders_top_rule_body_bottom_rule_and_keymap() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::IdeaInput;

        let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
        let rule = "─".repeat(200);
        let keymap = format!(
            "↑↓ move · Space expand · PgUp/PgDn page  ·  Enter input · : palette  ·  {}q quit",
            " ".repeat(122)
        );

        assert_eq!(
            lines,
            vec![
                "codexize─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────Idea Input · awaiting input",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &rule,
                &keymap,
            ]
        );
    }

    #[test]
    fn full_screen_brainstorm_running_renders_running_state_in_top_rule() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::BrainstormRunning;

        let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
        let rule = "─".repeat(200);
        let keymap = format!(
            "↑↓ move · Space expand · PgUp/PgDn page  ·  Enter input · : palette  ·  {}q quit",
            " ".repeat(122)
        );

        assert_eq!(
            lines,
            vec![
                "codexize─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────Brainstorming · running",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &rule,
                &keymap,
            ]
        );
    }

    /// Pads a modal content string to fit the dialog inner width and wraps it
    /// with the bordered block's vertical bars.
    fn modal_row(inner_width: u16, text: &str) -> String {
        let inner_width = inner_width as usize;
        let pad = inner_width.saturating_sub(text.chars().count());
        format!("│{}{}│", text, " ".repeat(pad))
    }

    #[test]
    fn spec_review_modal_is_centered_with_content_driven_height() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::SpecReviewPaused;

        let width = 100;
        let height = 30;
        let buf = render_full_frame_buf(&mut app, width, height);
        let dialog = expected_dialog_rect(width, height, 1);
        let inner_width = dialog.width.saturating_sub(2);

        assert_eq!(dialog, Rect::new(10, 13, 80, 4));
        assert!(raw_line_text(&buf, 0, width).contains("Spec Review · paused"));
        assert_eq!(
            raw_line_text(&buf, dialog.y, width),
            format!(
                "{}┌{}┐{}",
                " ".repeat(dialog.x as usize),
                "─".repeat(inner_width as usize),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
        assert_eq!(
            raw_line_text(&buf, dialog.y + 1, width),
            format!(
                "{}{}{}",
                " ".repeat(dialog.x as usize),
                modal_row(inner_width, "Spec review complete"),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
        assert!(
            raw_line_text(&buf, dialog.y + 2, width).contains("Enter continue"),
            "keymap should occupy the last inner row"
        );
        assert_eq!(
            raw_line_text(&buf, dialog.y + dialog.height - 1, width),
            format!(
                "{}└{}┘{}",
                " ".repeat(dialog.x as usize),
                "─".repeat(inner_width as usize),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
    }

    #[test]
    fn stage_error_modal_wraps_long_text_inside_centered_dialog() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::SpecReviewRunning;
        app.state.agent_error = Some(
            "model timeout while fetching a response from the remote reviewer after multiple retries"
                .to_string(),
        );

        let width = 100;
        let height = 30;
        let buf = render_full_frame_buf(&mut app, width, height);
        let dialog = expected_dialog_rect(width, height, 4);

        assert_eq!(dialog, Rect::new(10, 11, 80, 7));
        assert!(raw_line_text(&buf, dialog.y + 1, width).contains("Spec review failed"));
        assert_eq!(
            raw_line_text(&buf, dialog.y + 3, width),
            format!(
                "{}{}{}",
                " ".repeat(dialog.x as usize),
                modal_row(
                    dialog.width.saturating_sub(2),
                    "model timeout while fetching a response from the remote reviewer after"
                ),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
        assert_eq!(
            raw_line_text(&buf, dialog.y + 4, width),
            format!(
                "{}{}{}",
                " ".repeat(dialog.x as usize),
                modal_row(dialog.width.saturating_sub(2), "multiple retries"),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
        assert!(
            raw_line_text(&buf, dialog.y + 5, width).contains("r retry"),
            "keymap should remain visible after wrapped error content"
        );
    }

    #[test]
    fn skip_to_impl_modal_wraps_rationale_and_keeps_label_on_its_own_line() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::SkipToImplPending;
        app.state.skip_to_impl_rationale = Some(
            "This change only touches the centered dialog rendering path and preserves the existing footer controls."
                .to_string(),
        );

        let width = 100;
        let height = 30;
        let buf = render_full_frame_buf(&mut app, width, height);
        let dialog = expected_dialog_rect(width, height, 5);

        assert_eq!(dialog, Rect::new(10, 11, 80, 8));
        assert!(raw_line_text(&buf, dialog.y + 1, width).contains("The brainstorm agent proposes"));
        assert_eq!(
            raw_line_text(&buf, dialog.y + 3, width),
            format!(
                "{}{}{}",
                " ".repeat(dialog.x as usize),
                modal_row(dialog.width.saturating_sub(2), "Rationale: "),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
        assert_eq!(
            raw_line_text(&buf, dialog.y + 4, width)
                .trim()
                .trim_matches('│')
                .trim_end(),
            "This change only touches the centered dialog rendering path and preserves the"
        );
        assert_eq!(
            raw_line_text(&buf, dialog.y + 5, width)
                .trim()
                .trim_matches('│')
                .trim_end(),
            "existing footer controls."
        );
    }

    #[test]
    fn spec_review_modal_clamps_width_on_narrow_terminals() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::SpecReviewPaused;

        let width = 30;
        let height = 20;
        let buf = render_full_frame_buf(&mut app, width, height);
        let dialog = expected_dialog_rect(width, height, 1);

        assert_eq!(dialog, Rect::new(2, 8, 26, 4));
        assert_eq!(
            raw_line_text(&buf, dialog.y, width),
            format!(
                "{}┌{}┐{}",
                " ".repeat(dialog.x as usize),
                "─".repeat(dialog.width.saturating_sub(2) as usize),
                " ".repeat((width - dialog.x - dialog.width) as usize)
            )
        );
    }

    #[test]
    fn full_screen_implementation_round_2_with_active_live_summary() {
        let mut app = impl_round_2_running_app();

        let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
        let rule = "─".repeat(200);
        let keymap = format!(
            "↑↓ move · Space expand · PgUp/PgDn page  ·  Enter input · : palette  ·  {}q quit",
            " ".repeat(122)
        );

        assert_eq!(
            lines,
            vec![
                "codexize─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────[Run 42] · wiring full-screen tests",
                "▌▾ Implementation · running",
                " └─▾ Builder · running",
                "XX:XX:XX ⠋ Wiring full-screen tests",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &rule,
                &keymap,
            ]
        );
    }

    fn footer_line_count(lines: &[String]) -> usize {
        // Footer rows are non-empty rows below the bottom rule. The bottom rule
        // is the row immediately above either the status line (if present) or
        // the keymap. We count the trailing run of non-empty rows starting from
        // the keymap row upward.
        lines.iter().rev().take_while(|l| !l.is_empty()).count()
    }

    #[test]
    fn pushing_status_message_adds_one_extra_footer_line_then_ttl_hides_it() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::IdeaInput;

        let baseline = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
        let baseline_footer = footer_line_count(&baseline);

        // Push with a 5-second TTL so it survives the immediate `tick` inside draw().
        app.push_status(
            "transient status".to_string(),
            super::super::status_line::Severity::Warn,
            Duration::from_secs(5),
        );

        let with_status = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
        assert!(
            with_status.iter().any(|l| l.contains("transient status")),
            "status message visible in frame: {with_status:#?}"
        );
        assert_eq!(
            footer_line_count(&with_status),
            baseline_footer + 1,
            "status push adds exactly one footer line"
        );

        // TTL=0 forces immediate expiry on the next render's tick.
        app.push_status(
            "about to expire".to_string(),
            super::super::status_line::Severity::Warn,
            Duration::from_millis(0),
        );

        let after_expiry = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
        assert!(
            !after_expiry.iter().any(|l| l.contains("about to expire")),
            "expired status hidden: {after_expiry:#?}"
        );
        assert_eq!(
            footer_line_count(&after_expiry),
            baseline_footer,
            "footer shrinks back after TTL expiry"
        );
    }

    #[test]
    fn frame_status_line_severity_priority_info_then_error_wins() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::IdeaInput;

        app.push_status(
            "info first".to_string(),
            super::super::status_line::Severity::Info,
            Duration::from_secs(10),
        );
        app.push_status(
            "error wins".to_string(),
            super::super::status_line::Severity::Error,
            Duration::from_secs(10),
        );

        let lines = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
        assert!(lines.iter().any(|l| l.contains("error wins")));
        assert!(!lines.iter().any(|l| l.contains("info first")));
    }

    #[test]
    fn frame_status_line_severity_priority_error_then_info_keeps_error() {
        let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
        app.state.current_phase = Phase::IdeaInput;

        app.push_status(
            "error stays".to_string(),
            super::super::status_line::Severity::Error,
            Duration::from_secs(10),
        );
        app.push_status(
            "info ignored".to_string(),
            super::super::status_line::Severity::Info,
            Duration::from_secs(10),
        );

        let lines = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
        assert!(lines.iter().any(|l| l.contains("error stays")));
        assert!(!lines.iter().any(|l| l.contains("info ignored")));
    }

    #[test]
    fn push_status_routes_through_status_line_with_severity_priority() {
        let app = test_app(Vec::new(), Vec::new(), Vec::new());

        app.push_status(
            "info-msg".to_string(),
            super::super::status_line::Severity::Warn,
            Duration::from_secs(5),
        );
        let rendered = app
            .status_line
            .borrow()
            .render()
            .expect("status line should hold the warn message");
        assert_eq!(rendered.to_string(), "info-msg");

        // Lower severity must not silently overwrite a higher-severity message.
        app.push_status(
            "later-info".to_string(),
            super::super::status_line::Severity::Info,
            Duration::from_secs(5),
        );
        let still = app.status_line.borrow().render().unwrap();
        assert_eq!(still.to_string(), "info-msg");
    }

    /// Expanded penultimate sibling with visible children must render └─ for that
    /// sibling (since it's the last at its depth), not ├─.
    #[test]
    fn expanded_penultimate_sibling_renders_last_child_connector() {
        // Tree: Root (running) → Task A (first child), Task B (second/last child)
        // When Task A is expanded and has children, Task B should render └─.
        let nodes = vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![
                node(
                    "Task A",
                    NodeKind::Task,
                    NodeStatus::Running,
                    vec![node(
                        "Builder A",
                        NodeKind::Mode,
                        NodeStatus::Running,
                        Vec::new(),
                        Some(1),
                        None,
                    )],
                    None,
                    None,
                ),
                node(
                    "Task B",
                    NodeKind::Task,
                    NodeStatus::Pending,
                    Vec::new(),
                    None,
                    None,
                ),
            ],
            None,
            None,
        )];
        let app = test_app(nodes, vec![run_record(1, RunStatus::Running)], Vec::new());

        let lines = render_lines(&app, 10);

        // Task A has a child below it, but Task B follows Task A at the same depth,
        // so Task A should render ├─ (not last sibling).
        assert!(
            lines
                .iter()
                .any(|l| l.contains("├─") && l.contains("Task A")),
            "Task A should render ├─ (not last at its depth)"
        );
        // Task B is the last child at depth 1, so it should render └─.
        assert!(
            lines
                .iter()
                .any(|l| l.contains("└─") && l.contains("Task B")),
            "Task B should render └─ (last at its depth)"
        );
    }

    fn render_pipeline_buf(app: &App, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        PipelineWidget { app }.render(area, &mut buf);
        buf
    }

    fn row_text_count(lines: &[String], needle: &str) -> usize {
        lines.iter().filter(|line| line.contains(needle)).count()
    }

    fn flat_stage_app(labels: &[(&str, NodeStatus)]) -> App {
        test_app(
            labels
                .iter()
                .map(|(label, status)| {
                    node(label, NodeKind::Stage, *status, Vec::new(), None, None)
                })
                .collect(),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn sticky_running_stage_header_pins_above_scrolled_viewport() {
        let mut app = test_app(
            vec![node(
                "Running Stage",
                NodeKind::Stage,
                NodeStatus::Running,
                vec![
                    node(
                        "Task A",
                        NodeKind::Task,
                        NodeStatus::Done,
                        Vec::new(),
                        None,
                        None,
                    ),
                    node(
                        "Task B",
                        NodeKind::Task,
                        NodeStatus::Pending,
                        Vec::new(),
                        None,
                        None,
                    ),
                ],
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );
        app.viewport_top = 2;

        let buf = render_pipeline_buf(&app, 80, 2);

        assert!(
            line_text(&buf, 0, 80).contains("Running Stage"),
            "running stage header should pin to row 0"
        );
        let pinned_style = buf[(0, 0)].style();
        assert_eq!(
            pinned_style.bg,
            Some(Color::Reset),
            "pinned running stage header should not have a background fill"
        );
        assert!(
            pinned_style.add_modifier.contains(Modifier::UNDERLINED),
            "pinned running stage header should be underlined"
        );
        assert!(
            line_text(&buf, 1, 80).contains("Task B"),
            "normal scrolled content should start below the pinned row"
        );
    }

    #[test]
    fn sticky_running_stage_header_deactivates_while_naturally_visible() {
        let mut app = test_app(
            vec![node(
                "Running Stage",
                NodeKind::Stage,
                NodeStatus::Running,
                vec![node(
                    "Task A",
                    NodeKind::Task,
                    NodeStatus::Done,
                    Vec::new(),
                    None,
                    None,
                )],
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );
        app.viewport_top = 0;

        let lines = render_lines(&app, 4);

        assert_eq!(
            row_text_count(&lines, "Running Stage"),
            1,
            "naturally visible running stage must not be duplicated"
        );
        assert!(
            lines[1].contains("Task A"),
            "when unpinned, content should still begin at the natural next row"
        );
    }

    #[test]
    fn focus_scroll_uses_effective_height_with_sticky_header() {
        let mut app = flat_stage_app(&[
            ("Running Stage", NodeStatus::Running),
            ("One", NodeStatus::Done),
            ("Two", NodeStatus::Done),
            ("Three", NodeStatus::Done),
            ("Four", NodeStatus::Done),
            ("Focused", NodeStatus::Pending),
        ]);
        app.body_inner_height = 5;
        app.viewport_top = 1;
        app.selected = 5;
        app.selected_key = Some(app.visible_rows[5].key.clone());
        app.set_follow_tail(false);
        app.explicit_viewport_scroll = false;

        app.clamp_viewport();

        assert_eq!(
            app.viewport_top, 2,
            "row 0 is pinned, so the selected row needs a four-line content viewport"
        );
        let lines = render_lines(&app, app.body_inner_height as u16);
        assert!(
            lines[0].contains("Running Stage"),
            "running stage should remain pinned"
        );
        assert!(
            lines[4].contains("Focused"),
            "focused row should be visible below the pinned header"
        );
    }

    #[test]
    fn tail_follow_uses_effective_height_with_sticky_header() {
        let mut app = flat_stage_app(&[
            ("Running Stage", NodeStatus::Running),
            ("One", NodeStatus::Done),
            ("Two", NodeStatus::Done),
            ("Three", NodeStatus::Done),
            ("Four", NodeStatus::Done),
            ("Tail", NodeStatus::Pending),
        ]);
        app.body_inner_height = 5;

        app.clamp_viewport();

        assert_eq!(
            app.viewport_top, 2,
            "tail-follow should scroll to the bottom of the reduced content viewport"
        );
        assert_eq!(app.viewport_top, app.max_viewport_top());
        let lines = render_lines(&app, app.body_inner_height as u16);
        assert!(lines[0].contains("Running Stage"));
        assert!(lines[4].contains("Tail"));
    }

    #[test]
    fn depth_0_running_row_has_blue_background() {
        let app = test_app(
            vec![node(
                "Stage",
                NodeKind::Stage,
                NodeStatus::Running,
                Vec::new(),
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );

        let buf = render_pipeline_buf(&app, 80, 5);
        // Row 0 is the Running stage header.
        let style = buf[(0, 0)].style();
        assert_eq!(
            style.bg,
            Some(Color::Reset),
            "Running depth-0 row should not have a background fill"
        );
        assert!(
            style.add_modifier.contains(Modifier::UNDERLINED),
            "Running depth-0 row should be underlined"
        );
    }

    #[test]
    fn depth_0_done_row_has_green_background() {
        let app = test_app(
            vec![node(
                "Stage",
                NodeKind::Stage,
                NodeStatus::Done,
                Vec::new(),
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );

        let buf = render_pipeline_buf(&app, 80, 5);
        let style = buf[(0, 0)].style();
        assert_eq!(
            style.bg,
            Some(Color::Reset),
            "Done depth-0 row should not have a background fill"
        );
        assert!(
            style.add_modifier.contains(Modifier::UNDERLINED),
            "Done depth-0 row should be underlined"
        );
    }

    #[test]
    fn depth_0_failed_row_has_red_background() {
        let app = test_app(
            vec![node(
                "Stage",
                NodeKind::Stage,
                NodeStatus::Failed,
                Vec::new(),
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );

        let buf = render_pipeline_buf(&app, 80, 5);
        let style = buf[(0, 0)].style();
        assert_eq!(
            style.bg,
            Some(Color::Reset),
            "Failed depth-0 row should not have a background fill"
        );
        assert!(
            style.add_modifier.contains(Modifier::UNDERLINED),
            "Failed depth-0 row should be underlined"
        );
    }

    #[test]
    fn depth_0_failed_unverified_row_has_red_background() {
        let app = test_app(
            vec![node(
                "Stage",
                NodeKind::Stage,
                NodeStatus::FailedUnverified,
                Vec::new(),
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );

        let buf = render_pipeline_buf(&app, 80, 5);
        let style = buf[(0, 0)].style();
        assert_eq!(
            style.bg,
            Some(Color::Reset),
            "FailedUnverified depth-0 row should not have a background fill"
        );
        assert!(
            style.add_modifier.contains(Modifier::UNDERLINED),
            "FailedUnverified depth-0 row should be underlined"
        );
    }

    #[test]
    fn depth_0_pending_row_has_no_background() {
        let app = test_app(
            vec![node(
                "Stage",
                NodeKind::Stage,
                NodeStatus::Pending,
                Vec::new(),
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );

        let buf = render_pipeline_buf(&app, 80, 5);
        let style = buf[(0, 0)].style();
        // Pending rows should not have a status-colored background.
        assert!(
            !matches!(
                style.bg,
                Some(Color::Blue) | Some(Color::Green) | Some(Color::Red)
            ),
            "Pending depth-0 row should not have status background, got: {:?}",
            style.bg
        );
        assert!(
            style.add_modifier.contains(Modifier::UNDERLINED),
            "Pending depth-0 row should be underlined"
        );
    }

    #[test]
    fn depth_1_running_row_has_no_background_highlight() {
        // Background highlights are only for depth-0 rows.
        let app = test_app(
            vec![node(
                "Root",
                NodeKind::Stage,
                NodeStatus::Running,
                vec![node(
                    "Child",
                    NodeKind::Task,
                    NodeStatus::Running,
                    Vec::new(),
                    None,
                    None,
                )],
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );

        let lines = render_lines(&app, 5);
        let child_line_idx = lines
            .iter()
            .position(|l| l.contains("Child"))
            .expect("Child row present");

        let buf = render_pipeline_buf(&app, 80, 5);
        let bg = buf[(0, child_line_idx as u16)].style().bg;
        // Depth-1 rows should not have the stage background highlight.
        assert!(
            bg != Some(Color::Blue),
            "Depth-1 Running row should not have blue background"
        );
    }

    /// Builds a Root → Child fixture and renders a buffer wide enough that the
    /// child row's spans (focus glyph, indent, marker, label, separator, status
    /// label) all land within columns 0..=20. Returns the buffer and the y-row
    /// of the child line.
    fn render_depth_1_child(child_status: NodeStatus) -> (Buffer, u16) {
        let app = test_app(
            vec![node(
                "Root",
                NodeKind::Stage,
                NodeStatus::Done,
                vec![node(
                    "Child",
                    NodeKind::Task,
                    child_status,
                    Vec::new(),
                    None,
                    None,
                )],
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );
        let lines = render_lines(&app, 5);
        let child_line_idx = lines
            .iter()
            .position(|l| l.contains("Child"))
            .expect("Child row should be present in rendered lines")
            as u16;
        let buf = render_pipeline_buf(&app, 80, 5);
        (buf, child_line_idx)
    }

    /// At depth 1, the focus glyph (column 0) and indent (columns 1..=2) are
    /// excluded from the underline; every text-bearing column at or after the
    /// marker (column 3) must carry it.
    fn assert_depth_1_text_only_underline(buf: &Buffer, row: u16) {
        // Span 0: focus glyph at column 0 — never underlined.
        let focus_style = buf[(0, row)].style();
        assert!(
            !focus_style.add_modifier.contains(Modifier::UNDERLINED),
            "focus glyph cell at col 0 must not be underlined; style={focus_style:?}",
        );
        // Span 1: indent ("└─") at columns 1-2 — never underlined.
        for col in 1u16..=2 {
            let style = buf[(col, row)].style();
            assert!(
                !style.add_modifier.contains(Modifier::UNDERLINED),
                "indent cell at col {col} must not be underlined; style={style:?}",
            );
        }
        // Span 2 (marker "{glyph} ") and span 3 (label "Child") at columns
        // 3..=9 — every cell underlined.
        for col in 3u16..=9 {
            let style = buf[(col, row)].style();
            assert!(
                style.add_modifier.contains(Modifier::UNDERLINED),
                "text-span cell at col {col} should be underlined; style={style:?}",
            );
        }
        // Span 4 (" · ") at columns 10..=12 and span 5 (status label) at column
        // 13+ — both text-bearing, both underlined. Probe one cell from each.
        for col in [10u16, 13u16] {
            let style = buf[(col, row)].style();
            assert!(
                style.add_modifier.contains(Modifier::UNDERLINED),
                "text-span cell at col {col} should be underlined; style={style:?}",
            );
        }
    }

    #[test]
    fn depth_1_running_row_underlines_text_spans_only() {
        let (buf, row) = render_depth_1_child(NodeStatus::Running);
        assert_depth_1_text_only_underline(&buf, row);
    }

    #[test]
    fn depth_1_done_row_underlines_text_spans_only() {
        let (buf, row) = render_depth_1_child(NodeStatus::Done);
        assert_depth_1_text_only_underline(&buf, row);
    }

    #[test]
    fn depth_1_failed_row_underlines_text_spans_only() {
        let (buf, row) = render_depth_1_child(NodeStatus::Failed);
        assert_depth_1_text_only_underline(&buf, row);
    }

    #[test]
    fn depth_1_failed_unverified_row_underlines_text_spans_only() {
        let (buf, row) = render_depth_1_child(NodeStatus::FailedUnverified);
        assert_depth_1_text_only_underline(&buf, row);
    }

    #[test]
    fn depth_1_pending_row_has_no_underline_anywhere() {
        let (buf, row) = render_depth_1_child(NodeStatus::Pending);
        // Pending rows must not carry the modifier on any cell — neither on
        // structural spans nor on text spans.
        for col in 0u16..20 {
            let style = buf[(col, row)].style();
            assert!(
                !style.add_modifier.contains(Modifier::UNDERLINED),
                "pending depth-1 row should not be underlined at col {col}; \
                 style={style:?}",
            );
        }
    }

    #[test]
    fn depth_1_loop_summary_spans_underlined_when_present() {
        // A child labelled "Loop" with non-empty summary appends spans 6
        // (" · ") and 7 (summary text). Both must carry the modifier; the
        // focus glyph and indent must not.
        let app = test_app(
            vec![node(
                "Root",
                NodeKind::Stage,
                NodeStatus::Done,
                vec![node(
                    "Loop",
                    NodeKind::Task,
                    NodeStatus::Running,
                    Vec::new(),
                    None,
                    None,
                )],
                None,
                None,
            )],
            Vec::new(),
            Vec::new(),
        );
        let lines = render_lines(&app, 5);
        let row = lines
            .iter()
            .position(|l| l.contains("Loop summary"))
            .expect("Loop summary should appear on the child row") as u16;
        let buf = render_pipeline_buf(&app, 80, 5);

        // Spans 0 & 1 stay un-underlined regardless of the Loop summary.
        for col in 0u16..=2 {
            let style = buf[(col, row)].style();
            assert!(
                !style.add_modifier.contains(Modifier::UNDERLINED),
                "structural cell at col {col} should not be underlined; \
                 style={style:?}",
            );
        }

        // Layout for "└─▸ Loop · running · Loop summary":
        //   cols 0     focus glyph " "
        //   cols 1-2   indent "└─"
        //   cols 3-4   marker "▸ "
        //   cols 5-8   label "Loop"
        //   cols 9-11  " · "
        //   cols 12-18 "running"
        //   cols 19-21 " · "       ← span 6
        //   cols 22-33 "Loop summary" ← span 7
        //
        // Probe a cell inside each Loop summary span.
        for col in [20u16, 25u16] {
            let style = buf[(col, row)].style();
            assert!(
                style.add_modifier.contains(Modifier::UNDERLINED),
                "Loop summary cell at col {col} should be underlined; \
                 style={style:?}",
            );
        }
    }
}
