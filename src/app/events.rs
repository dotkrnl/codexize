use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::{NodeStatus, Phase};

use super::{App, ExpansionOverride};

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        if self.input_mode {
            return self.handle_input_key(key);
        }

        if self.state.current_phase == Phase::SkipToImplPending {
            return self.handle_skip_to_impl_modal_key(key);
        }

        if self.confirm_back && key.code != KeyCode::Char('b') {
            self.confirm_back = false;
            return false;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('b') => {
                if self.confirm_back {
                    self.confirm_back = false;
                    self.go_back();
                } else if self.can_go_back() {
                    self.confirm_back = true;
                }
                false
            }
            KeyCode::Up => {
                self.scroll_or_move_focus(-1);
                false
            }
            KeyCode::Down => {
                self.scroll_or_move_focus(1);
                false
            }
            KeyCode::Char(' ') => {
                self.toggle_expand_focused();
                false
            }
            KeyCode::Enter => {
                let on_current = self.selected == self.current_row();
                if on_current {
                    if self.state.current_phase == Phase::SpecReviewPaused {
                        let _ = self.transition_to_phase(Phase::SpecReviewRunning);
                        self.launch_spec_review();
                        return false;
                    }
                    if self.state.current_phase == Phase::PlanReviewPaused {
                        let _ = self.transition_to_phase(Phase::PlanReviewRunning);
                        self.launch_plan_review();
                        return false;
                    }
                    if self.state.current_phase == Phase::BrainstormRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        let idea = self.state.idea_text.clone().unwrap_or_default();
                        self.launch_brainstorm(idea);
                        return false;
                    }
                    if self.state.current_phase == Phase::SpecReviewRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_spec_review();
                        return false;
                    }
                    if self.state.current_phase == Phase::PlanningRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_planning();
                        return false;
                    }
                    if self.state.current_phase == Phase::PlanReviewRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_plan_review();
                        return false;
                    }
                    if self.state.current_phase == Phase::ShardingRunning
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_sharding();
                        return false;
                    }
                    if matches!(self.state.current_phase, Phase::ImplementationRound(_))
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_coder();
                        return false;
                    }
                    if matches!(self.state.current_phase, Phase::ReviewRound(_))
                        && (self.state.agent_error.is_some() || !self.window_launched)
                    {
                        self.launch_reviewer();
                        return false;
                    }
                }

                if self.can_focus_input() {
                    self.input_mode = true;
                }
                // Expand/collapse is Space only (spec § Navigation System); Enter no
                // longer toggles the body.
                false
            }
            KeyCode::Char('n') => {
                let can_skip_spec = self.state.current_phase == Phase::SpecReviewPaused
                    || (self.state.current_phase == Phase::SpecReviewRunning
                        && self.state.agent_error.is_some());
                let can_skip_plan = self.state.current_phase == Phase::PlanReviewPaused
                    || (self.state.current_phase == Phase::PlanReviewRunning
                        && self.state.agent_error.is_some());
                if can_skip_spec {
                    self.state.agent_error = None;
                    self.queue_view_of_current_artifact("spec.md");
                    let _ = self.transition_to_phase(Phase::PlanningRunning);
                } else if can_skip_plan {
                    self.state.agent_error = None;
                    self.queue_view_of_current_artifact("plan.md");
                    let _ = self.transition_to_phase(Phase::ShardingRunning);
                }
                false
            }
            KeyCode::Char('e') => {
                self.open_editable_artifact();
                false
            }
            KeyCode::Char('t') => false,
            KeyCode::PageUp => {
                let step = self.body_inner_height.saturating_sub(1).max(1) as isize;
                self.scroll_viewport(-step, true);
                false
            }
            KeyCode::PageDown => {
                let step = self.body_inner_height.saturating_sub(1).max(1) as isize;
                self.scroll_viewport(step, true);
                false
            }
            _ => false,
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = false;
                return false;
            }
            KeyCode::Enter => {
                let trimmed = self.input_buffer.trim().to_string();
                if !trimmed.is_empty() {
                    if trimmed == "/exit" {
                        return true;
                    }

                    if trimmed == "/stats" || trimmed == "/status" || trimmed == "/usage" {
                        self.force_refresh_models();
                        self.input_buffer.clear();
                        self.input_cursor = 0;
                        self.input_mode = false;
                        return false;
                    }

                    if self.state.current_phase == Phase::IdeaInput {
                        self.input_buffer.clear();
                        self.input_cursor = 0;
                        self.input_mode = false;
                        self.launch_brainstorm(trimmed);
                        return false;
                    }

                    self.input_buffer.clear();
                    self.input_cursor = 0;
                }
                self.input_mode = false;
                return false;
            }
            _ => {}
        }
        let _ = crate::input_editor::apply(&mut self.input_buffer, &mut self.input_cursor, key);
        false
    }

    pub(super) fn toggle_expand_focused(&mut self) {
        let Some(row) = self.visible_rows.get(self.selected).cloned() else {
            return;
        };
        if row.status == NodeStatus::Pending || !row.is_expandable() {
            return;
        }
        let default_expanded = self.default_expanded(&row);
        let desired = !self.is_expanded(self.selected);
        let next_override = match (desired, default_expanded) {
            (true, true) | (false, false) => None,
            (true, false) => Some(ExpansionOverride::Expanded),
            (false, true) => Some(ExpansionOverride::Collapsed),
        };
        match next_override {
            Some(value) => {
                self.collapsed_overrides.insert(row.key.clone(), value);
            }
            None => {
                self.collapsed_overrides.remove(&row.key);
            }
        }
        self.rebuild_visible_rows();
        self.restore_selection(Some(row.key), self.selected);
    }

    pub(super) fn scroll_or_move_focus(&mut self, delta: isize) {
        let idx = self.selected;
        let area_h = self.body_inner_height;
        let (ys, total) = self.header_y_offsets();
        let Some(&header_y) = ys.get(idx) else {
            self.move_focus(delta);
            return;
        };
        let next_y = ys.get(idx + 1).copied().unwrap_or(total);
        let section_bottom = next_y; // exclusive end of selected row's content block

        if delta < 0 {
            if self.viewport_top > header_y {
                self.scroll_viewport(-1, false);
            } else {
                self.move_focus(delta);
            }
        } else if area_h > 0 && self.viewport_top + area_h < section_bottom {
            self.scroll_viewport(1, false);
        } else {
            self.move_focus(delta);
        }
    }

    fn move_focus(&mut self, delta: isize) {
        self.explicit_viewport_scroll = false;
        if delta < 0 {
            // Any upward focus action also breaks tail-follow so the user can
            // read history without the viewport yanking back to the latest.
            self.set_follow_tail(false);
            self.selected = self.selected.saturating_sub(1);
        } else if self.selected + 1 < self.visible_rows.len() {
            self.selected += 1;
        }
        self.selected_key = self
            .visible_rows
            .get(self.selected)
            .map(|row| row.key.clone());
    }

    fn handle_skip_to_impl_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if let Err(err) = self.accept_skip_to_implementation() {
                    self.state.agent_error =
                        Some(format!("accept skip-to-implementation failed: {err:#}"));
                }
                false
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                if let Err(err) = self.decline_skip_to_implementation() {
                    self.state.agent_error =
                        Some(format!("decline skip-to-implementation failed: {err:#}"));
                }
                false
            }
            _ => false,
        }
    }
}
