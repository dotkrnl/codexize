use ratatui::{
    Frame,
    buffer::Buffer,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use crate::state::{NodeStatus, Phase, RunRecord, RunStatus};
use crate::tui::wrap_input;
use chrono::Offset;
use std::collections::BTreeSet;

#[cfg(test)]
use super::state::ModelRefreshState;
mod input_sheet;
mod pipeline;
mod split_view;

use self::pipeline::PipelineWidget;
use self::split_view::SplitWidget;

use super::{
    App, ModalKind,
    chrome::{UnreadBadge, bottom_rule, modal::render_modal_overlay, top_rule_with_left_spans},
    clock::Clock,
    focus_caps::FocusCaps,
    footer::{
        CachedSummaryFetcher, TranscriptLeafMarker, extract_short_title,
        format_running_transcript_leaf, format_stalled_transcript_leaf, keymap,
    },
    models_area,
    render_view_model::{
        guard_content, is_last_sibling, modal_border_style, modal_title, skip_to_impl_content,
        spinner_frame, stage_error_content, status_highlight_bg,
    },
    sheet::bottom_sheet_without_rule,
};

pub use super::render_view_model::sanitize_live_summary;

const DEGENERATE_FLOOR: u16 = 16;
const BODY_FLOOR_NORMAL: u16 = 8;

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
        let modal = self.active_modal();

        let caps = self.focus_caps();
        let split_open = self.is_split_open();
        let split_owns_input = self.split_owns_input();

        let input_surface_active = !split_owns_input
            && (if self.interactive_run_active() {
                self.interactive_run_waiting_for_input()
            } else {
                self.input_mode
            });
        let keymap_line = keymap(
            self.state.current_phase,
            modal,
            caps,
            input_surface_active || split_owns_input,
            split_open,
            width,
        );

        // Sheet content is owned by the input-mode path only. Modal content
        // is computed independently inside the overlay branch below.
        let sheet_content: Option<Vec<Line<'static>>> = if input_surface_active {
            Some(self.input_sheet_content(width))
        } else {
            None
        };

        // Footer height: only the input-mode sheet (when active) plus the
        // always-present keymap+status lines contribute. Modal state is
        // overlaid and does not change body height.
        let footer_h = if let Some(ref content) = sheet_content {
            // The app chrome already draws the divider rule above the sheet,
            // so footer rows are just content + controls.
            let desired = (content.len() as u16).saturating_add(1);
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
        self.split_fullscreen = split_open && term_h <= super::RESPONSIVE_HEIGHT_THRESHOLD;
        self.latch_visible_expansions();
        self.clamp_viewport();
        self.clamp_split_scroll(self.current_split_content_height());
        self.live_summary_spinner_visible =
            self.live_summary_spinner_visible_for_height(body_h as usize);

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

        // 3. Pipeline body & Split
        if body_h > 0 {
            let body_area = ratatui::layout::Rect::new(area.x, y, width, body_h);
            if split_open {
                if self.split_fullscreen {
                    frame.render_widget(SplitWidget { app: self }, body_area);
                } else {
                    let tree_h = body_h / 3;
                    let split_h = body_h.saturating_sub(tree_h);
                    let tree_area = ratatui::layout::Rect::new(area.x, y, width, tree_h);
                    let split_area = ratatui::layout::Rect::new(area.x, y + tree_h, width, split_h);
                    frame.render_widget(PipelineWidget { app: self }, tree_area);
                    frame.render_widget(SplitWidget { app: self }, split_area);
                }
            } else {
                frame.render_widget(PipelineWidget { app: self }, body_area);
            }
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
            let max_w = terminal_width.saturating_sub(4).max(1);
            let dialog_w = max_w.min(80).max(max_w.min(40));
            let inner_w = dialog_w.saturating_sub(2);
            let content = self.modal_content_lines(m, inner_w);
            let modal_keymap = keymap(
                self.state.current_phase,
                Some(m),
                caps,
                false,
                false,
                inner_w,
            );
            render_modal_overlay(
                frame,
                area,
                modal_title(m),
                modal_border_style(m),
                content,
                modal_keymap,
            );
        } else if let Some(content) = sheet_content {
            let sheet_lines = bottom_sheet_without_rule(content, keymap_line, footer_h);
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
            let overlay_h = self.palette_overlay_height(area.height, modal.is_some());
            if overlay_h > 0 {
                let overlay = ratatui::layout::Rect::new(
                    area.x,
                    area.y + area.height.saturating_sub(overlay_h),
                    area.width,
                    overlay_h,
                );
                frame.render_widget(Clear, overlay);
                let lines = self.palette_overlay_lines(width, overlay_h);
                frame.render_widget(Paragraph::new(lines), overlay);
            }
        }
    }

    /// Compute the bottom-aligned palette overlay height.
    ///
    /// The input row is mandatory; suggestion rows clamp before reaching
    /// the body floor so modal and bottom-sheet controls remain reachable
    /// on short terminals (per spec). When a modal is active, conservatively
    /// stick to a 2-row overlay so the centered modal keymap is not covered.
    fn palette_overlay_height(&self, area_height: u16, modal_active: bool) -> u16 {
        if area_height == 0 {
            return 0;
        }
        if modal_active {
            return area_height.min(2);
        }
        // Reserve a body floor above the overlay so the body/top chrome stays
        // visible. The floor must remain low enough that very short terminals
        // still get at least the input row.
        const BODY_RESERVE: u16 = 4;
        const MAX_OVERLAY: u16 = 12; // input + up to 10 suggestions + help

        let commands = self.palette_commands();
        let filtered = super::palette::filter(&self.palette.buffer, &commands);
        let suggestions = filtered.len().min(10) as u16;
        let desired = 1 + suggestions + 1; // input + suggestions + help

        let cap = area_height.saturating_sub(BODY_RESERVE).max(1);
        desired.min(cap).clamp(1, MAX_OVERLAY)
    }

    fn palette_overlay_lines(&self, width: u16, max_h: u16) -> Vec<Line<'static>> {
        if max_h == 0 || width == 0 {
            return Vec::new();
        }
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
            Span::raw(buffer.clone()),
        ];
        if !suffix.is_empty() {
            input_spans.push(Span::styled(
                suffix.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let mut lines: Vec<Line<'static>> = vec![Line::from(input_spans)];
        let max = max_h as usize;

        let help_text = "Esc close  Tab complete  Enter run";
        let help_fits = max >= 2 && (width as usize) >= help_text.chars().count();
        let help_reserve = if help_fits { 1 } else { 0 };
        let suggestion_capacity = max.saturating_sub(1).saturating_sub(help_reserve);

        let filtered = super::palette::filter(&buffer, &commands);
        for cmd in filtered.iter().take(suggestion_capacity) {
            let text = super::palette::suggestion_text(cmd, width);
            lines.push(Line::from(Span::styled(
                text,
                Style::default().fg(Color::Gray),
            )));
        }

        if help_fits && lines.len() < max {
            let mut help = help_text.to_string();
            if width < help.chars().count() as u16 {
                help.truncate(width as usize);
            }
            lines.push(Line::from(Span::styled(
                help,
                Style::default().fg(Color::DarkGray),
            )));
        }

        lines
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
            can_split: self.resolve_split_target_for_selected_row().is_some(),
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
}

#[cfg(test)]
mod tests_mod;
