use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::{NodeStatus, Phase};

use super::{App, tree::current_node_index};

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
                let on_current = self.selected == current_node_index(&self.nodes);
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
                    let _ = self.transition_to_phase(Phase::PlanningRunning);
                } else if can_skip_plan {
                    self.state.agent_error = None;
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
                self.scroll_selected(-(self.page_step() as isize));
                false
            }
            KeyCode::PageDown => {
                self.scroll_selected(self.page_step() as isize);
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
        let _ = crate::input_editor::apply(
            &mut self.input_buffer,
            &mut self.input_cursor,
            key,
        );
        false
    }

    pub(super) fn toggle_expand_focused(&mut self) {
        let current = current_node_index(&self.nodes);
        // The currently-running stage is always implicitly expanded; don't let the user
        // explicitly collapse it.
        if self.selected == current {
            return;
        }
        if self.nodes[self.selected].status == NodeStatus::Pending {
            return;
        }
        let Some(key) = self
            .nodes
            .get(self.selected)
            .and_then(Self::stage_scroll_key)
        else {
            return;
        };
        if !self.collapsed_overrides.insert(key.clone()) {
            self.collapsed_overrides.remove(&key);
        }
    }

    pub(super) fn scroll_or_move_focus(&mut self, delta: isize) {
        let idx = self.selected;
        if !self.is_expanded(idx) {
            self.move_focus(delta);
            return;
        }
        let max_offset = self.stage_max_offset(idx);
        let current = self.effective_stage_scroll(idx, max_offset);
        if delta < 0 {
            if current > 0 {
                self.set_stage_scroll(idx, current.saturating_sub(1));
            } else {
                self.boundary_handoff(delta);
            }
        } else if current < max_offset {
            self.set_stage_scroll(idx, current + 1);
        } else {
            self.boundary_handoff(delta);
        }
    }

    fn move_focus(&mut self, delta: isize) {
        if delta < 0 {
            self.selected = self.selected.saturating_sub(1);
        } else if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
    }

    fn boundary_handoff(&mut self, delta: isize) {
        // Prefer jumping to the next expanded stage in the direction, skipping any
        // collapsed stages between (spec § Navigation System → Boundary Handling).
        if let Some(target) = self.find_expanded_neighbor(self.selected, delta) {
            self.selected = target;
            let max_offset = self.stage_max_offset(self.selected);
            let landing = if delta < 0 { max_offset } else { 0 };
            self.set_stage_scroll(self.selected, landing);
            return;
        }
        // No expanded neighbor in that direction — fall back to a single-step focus
        // move, which may land on a collapsed stage (focus-only move per spec).
        self.move_focus(delta);
    }

    fn find_expanded_neighbor(&self, from: usize, delta: isize) -> Option<usize> {
        if delta < 0 {
            (0..from).rev().find(|&i| self.is_expanded(i))
        } else {
            ((from + 1)..self.nodes.len()).find(|&i| self.is_expanded(i))
        }
    }

    fn scroll_selected(&mut self, delta: isize) {
        let idx = self.selected;
        if !self.is_expanded(idx) {
            return;
        }
        let max_offset = self.stage_max_offset(idx) as isize;
        let current = self.effective_stage_scroll(idx, max_offset as usize) as isize;
        let next = (current + delta).clamp(0, max_offset);
        self.set_stage_scroll(idx, next as usize);
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

    pub(super) fn clamp_scroll(&mut self) {
        // Preserve the usize::MAX "stick to bottom" sentinel; clamp concrete offsets
        // against the current per-stage max so tree rebuilds don't leave them stale.
        // Collapsed stages are skipped: their max_offset is only meaningful when the
        // body is rendered, and clamping them here would erase a user's stored offset
        // across a collapse/re-expand cycle (spec § State Persistence).
        let max_offsets: Vec<(String, usize)> = (0..self.nodes.len())
            .filter(|&i| self.is_expanded(i))
            .filter_map(|index| {
                let key = Self::stage_scroll_key(&self.nodes[index])?;
                Some((key, self.stage_max_offset(index)))
            })
            .collect();
        for (key, max_offset) in max_offsets {
            if let Some(scroll) = self.stage_scroll.get_mut(&key) {
                if *scroll != usize::MAX && *scroll > max_offset {
                    *scroll = max_offset;
                }
            }
        }
    }
}
