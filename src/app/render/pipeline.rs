use super::*;
use crate::app::chat_widget;
use crate::app::clock::WallClock;
use crate::app::split::{SplitTarget, run_main_panel_message_visible};

pub(super) struct PipelineWidget<'a> {
    pub(super) app: &'a App,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipelineLineKind {
    Other,
    RunningLeafTail { run_id: u64 },
    RunningContainerPlaceholder { run_id: u64 },
}

struct PipelineLine {
    line: Line<'static>,
    kind: PipelineLineKind,
}

pub(super) struct RunningTailLine {
    pub(super) line: Line<'static>,
    kind: PipelineLineKind,
}

impl Widget for PipelineWidget<'_> {
    fn render(self, area: ratatui::layout::Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let area_h = area.height as usize;
        let viewport_top = self
            .app
            .viewport_top
            .min(self.app.max_viewport_top_for_height(area_h));
        let pinned_header = self.app.pinned_running_header(viewport_top);

        let visible_tail_runs = self
            .app
            .visible_live_summary_tail_runs(area_h, viewport_top);
        let lines = self.app.pipeline_render_lines(&visible_tail_runs);
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
        for (offset, rendered) in lines[viewport_top..end].iter().enumerate() {
            buf.set_line(
                area.x,
                content_y + offset as u16,
                &rendered.line,
                area.width,
            );
        }
    }
}

impl App {
    fn live_agent_progress_recent(&self) -> bool {
        const STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
        if self
            .agent_last_change
            .is_some_and(|last| last.elapsed() <= STALL_TIMEOUT)
        {
            return true;
        }
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.status == RunStatus::Running)
            .any(|run| {
                chrono::Utc::now()
                    .signed_duration_since(run.started_at)
                    .to_std()
                    .map(|age| age <= STALL_TIMEOUT)
                    .unwrap_or(true)
            })
    }

    pub(super) fn live_agent_spinner_active(&self) -> bool {
        self.state
            .agent_runs
            .iter()
            .any(|run| run.status == RunStatus::Running)
    }

    fn pipeline_render_lines(
        &self,
        suppressed_container_runs: &BTreeSet<u64>,
    ) -> Vec<PipelineLine> {
        let mut lines = Vec::new();
        for index in 0..self.visible_rows.len() {
            let Some(node) = self.node_for_row(index) else {
                continue;
            };
            let expanded = self.is_expanded(index);
            lines.push(PipelineLine {
                line: self.node_header(index, expanded, node),
                kind: PipelineLineKind::Other,
            });
            if expanded && self.is_expanded_body(index) {
                lines.extend(self.node_body_for_render(index, suppressed_container_runs));
            }
        }
        lines
    }

    pub(super) fn visible_live_summary_tail_runs(
        &self,
        area_h: usize,
        viewport_top: usize,
    ) -> BTreeSet<u64> {
        if area_h == 0 {
            return BTreeSet::new();
        }
        let candidate_lines = self.pipeline_render_lines(&BTreeSet::new());
        let content_h = if self.pinned_running_header(viewport_top).is_some() {
            area_h.saturating_sub(1)
        } else {
            area_h
        };
        let end = (viewport_top + content_h).min(candidate_lines.len());
        candidate_lines[viewport_top..end]
            .iter()
            .filter_map(|line| match line.kind {
                PipelineLineKind::RunningLeafTail { run_id } => Some(run_id),
                _ => None,
            })
            .collect()
    }

    pub(super) fn live_summary_spinner_visible_for_height(&self, area_h: usize) -> bool {
        if !self.live_agent_spinner_active() {
            return false;
        }

        // If the split is showing a running run, the spinner should be visible.
        if let Some(SplitTarget::Run(run_id)) = self.split_target
            && self
                .state
                .agent_runs
                .iter()
                .any(|run| run.id == run_id && run.status == RunStatus::Running)
        {
            return true;
        }

        let viewport_top = self
            .viewport_top
            .min(self.max_viewport_top_for_height(area_h));
        !self
            .visible_live_summary_tail_runs(area_h, viewport_top)
            .is_empty()
    }

    pub(crate) fn split_running_tail_line(&self, run: &RunRecord) -> Option<Line<'static>> {
        // The split still derives the tail shape from the selected pipeline row.
        // Keep lifecycle line counts on the same path until split targets carry
        // their own row identity.
        let suppressed_container_runs =
            self.visible_live_summary_tail_runs(self.split_viewport_height(), self.viewport_top);
        self.running_tail_for_row(
            self.selected,
            run,
            &crate::app::clock::WallClock::new(),
            &suppressed_container_runs,
        )
        .map(|tail| tail.line)
    }

    pub(super) fn node_header(
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

        // Structural focus marker: `▌` follows the persistent section color gutter.
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

        let dim = Style::default().fg(Color::DarkGray);
        let color_block_style = match status_highlight_bg(node.status) {
            Some(bg) => Style::default().bg(bg),
            None => Style::default(),
        };

        let mut spans = vec![
            Span::styled(" ", color_block_style),
            Span::styled(focus_glyph, Style::default()),
            Span::styled(indent, dim),
            Span::raw(" "),
            Span::raw(marker.to_string()),
            Span::raw(" "),
            Span::raw(node.label.clone()),
            Span::styled(" · ", dim),
            Span::styled(node.status.label(), node.status.style()),
        ];
        if node.label == "Loop" && !node.summary.is_empty() {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(node.summary.clone(), dim));
        }

        Line::from(spans).style(style)
    }

    pub(in crate::app) fn node_body(&self, index: usize) -> Vec<Line<'static>> {
        let width = self.body_inner_width.max(1);
        let local_offset = chrono::Local::now().fixed_offset().offset().fix();
        self.node_body_lines_with_offset(index, width, &local_offset, &BTreeSet::new())
            .into_iter()
            .map(|rendered| rendered.line)
            .collect()
    }

    fn node_body_for_render(
        &self,
        index: usize,
        suppressed_container_runs: &BTreeSet<u64>,
    ) -> Vec<PipelineLine> {
        let width = self.body_inner_width.max(1);
        let local_offset = chrono::Local::now().fixed_offset().offset().fix();
        self.node_body_lines_with_offset(index, width, &local_offset, suppressed_container_runs)
    }

    fn node_body_lines_with_offset(
        &self,
        index: usize,
        available_width: usize,
        local_offset: &chrono::FixedOffset,
        suppressed_container_runs: &BTreeSet<u64>,
    ) -> Vec<PipelineLine> {
        let Some(node) = self.node_for_row(index) else {
            return Vec::new();
        };

        if let Some(id) = node.run_id.or(node.leaf_run_id)
            && let Some(run) = self.state.agent_runs.iter().find(|r| r.id == id)
        {
            // Main panel restores the system/status/user transcript surface
            // for both interactive and non-interactive runs. ACP output
            // (`AgentText`) and thought/tool text (`AgentThought`) stay in
            // the split panel — the visibility decision is centralized in
            // `run_main_panel_message_visible`.
            let msgs: Vec<_> = self
                .messages
                .iter()
                .filter(|m| m.run_id == id)
                .filter(|m| {
                    run_main_panel_message_visible(run, m.kind, self.state.show_thinking_texts)
                })
                .cloned()
                .collect();
            let running_tail = self.running_tail_for_row(
                index,
                run,
                &WallClock::new(),
                suppressed_container_runs,
            );
            let tail_kind = running_tail.as_ref().map(|tail| tail.kind);
            let has_end = msgs
                .iter()
                .any(|m| m.kind == crate::state::MessageKind::End);
            let mut lines: Vec<_> = chat_widget::message_lines(
                &msgs,
                run,
                local_offset,
                running_tail.map(|tail| tail.line),
                available_width,
            )
            .into_iter()
            .map(|line| PipelineLine {
                line,
                kind: PipelineLineKind::Other,
            })
            .collect();
            if run.status == RunStatus::Running
                && !has_end
                && let Some(kind) = tail_kind
                && let Some(last) = lines.last_mut()
            {
                last.kind = kind;
            }
            return lines;
        }

        self.render_compact_node(node, index)
            .into_iter()
            .map(|line| PipelineLine {
                line,
                kind: PipelineLineKind::Other,
            })
            .collect()
    }

    /// Choose the trailing line that closes a still-running transcript body.
    ///
    /// Per spec, leaf transcript rows render the tail as a "live agent
    /// message" (`HH:MM:SS ⠋ live-summary-title`). Container rows use the
    /// tree-shape placeholder only when a visible child transcript tail for the
    /// same run is not already representing progress.
    pub(super) fn running_tail_for_row<C: Clock>(
        &self,
        index: usize,
        run: &RunRecord,
        clock: &C,
        suppressed_container_runs: &BTreeSet<u64>,
    ) -> Option<RunningTailLine> {
        if run.status != RunStatus::Running {
            return None;
        }
        if !self.live_agent_spinner_active() {
            return None;
        }
        if run.modes.interactive
            && self.interactive_run_waiting_for_input()
            && self
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.run_id == run.id
                        && matches!(
                            message.kind,
                            crate::state::MessageKind::AgentText
                                | crate::state::MessageKind::AgentThought
                                | crate::state::MessageKind::UserInput
                        )
                })
                .is_some_and(|message| message.kind == crate::state::MessageKind::AgentText)
        {
            return None;
        }
        let row = self.visible_rows.get(index)?;
        if row.has_children {
            if suppressed_container_runs.contains(&run.id) {
                return None;
            }
            let recent = self.live_agent_progress_recent();
            let spin = if recent {
                spinner_frame(self.spinner_tick)
            } else {
                spinner_frame(0)
            };
            let label = if recent { "running" } else { "stalled" };
            let dim = Style::default().fg(Color::DarkGray);
            let gutter = "│ ".repeat(row.depth);
            return Some(RunningTailLine {
                line: Line::from(vec![
                    Span::styled(format!(" {gutter}  "), dim),
                    Span::styled(
                        spin,
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {label}"), dim),
                ]),
                kind: PipelineLineKind::RunningContainerPlaceholder { run_id: run.id },
            });
        }
        let phase_label = self.state.current_phase.label();
        let fetcher = CachedSummaryFetcher::new(&self.live_summary_cached_text, &phase_label);
        let line = if self.live_agent_progress_recent() {
            format_running_transcript_leaf(
                TranscriptLeafMarker::new(),
                clock,
                self.spinner_tick,
                &fetcher,
            )
        } else {
            format_stalled_transcript_leaf(TranscriptLeafMarker::new(), clock, &fetcher)
        };
        Some(RunningTailLine {
            line,
            kind: PipelineLineKind::RunningLeafTail { run_id: run.id },
        })
    }

    fn render_compact_node(&self, node: &crate::state::Node, index: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let depth = self.visible_rows.get(index).map_or(0, |r| r.depth);
        let gutter = "│ ".repeat(depth);
        let dim = Style::default().fg(Color::DarkGray);

        if node.status == NodeStatus::Running
            && self.run_launched
            && self.live_agent_spinner_active()
        {
            let recent = self.live_agent_progress_recent();
            let spin = if recent {
                spinner_frame(self.spinner_tick)
            } else {
                spinner_frame(0)
            };
            let label = if recent { "running" } else { "stalled" };
            lines.push(Line::from(vec![
                Span::styled(format!(" {gutter}  "), dim),
                Span::styled(
                    spin,
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {label} · {} lines", self.agent_line_count), dim),
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
        lines
    }
}
