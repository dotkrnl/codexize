use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::Phase;

use super::{
    App,
    sections::{build_sections, current_section_index},
};

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        if self.input_mode {
            return self.handle_input_key(key);
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
                self.selected = self.selected.saturating_sub(1);
                false
            }
            KeyCode::Down => {
                if self.selected + 1 < self.sections.len() {
                    self.selected += 1;
                }
                false
            }
            KeyCode::Enter => {
                let on_current = self.selected == self.current_section();
                if on_current {
                    if self.state.current_phase == Phase::SpecReviewPaused {
                        let _ = self.state.transition_to(Phase::SpecReviewRunning);
                        self.launch_spec_review();
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
                } else {
                    self.toggle_selected_section();
                }
                false
            }
            KeyCode::Char('n') => {
                let can_skip = self.state.current_phase == Phase::SpecReviewPaused
                    || (self.state.current_phase == Phase::SpecReviewRunning
                        && self.state.agent_error.is_some());
                if can_skip {
                    self.state.agent_error = None;
                    let _ = self.state.transition_to(Phase::PlanningRunning);
                    self.sections = build_sections(&self.state, self.window_launched);
                    self.section_scroll.resize(self.sections.len(), usize::MAX);
                    self.selected = self.sections.iter()
                        .position(|s| s.name == "Planning")
                        .unwrap_or_else(|| current_section_index(&self.sections));
                }
                false
            }
            KeyCode::Char('e') => {
                self.open_editable_artifact();
                false
            }
            KeyCode::Char('t') => {
                self.toggle_transcript();
                false
            }
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
                false
            }
            KeyCode::Enter => {
                let trimmed = self.input_buffer.trim().to_string();
                if !trimmed.is_empty() {
                    if trimmed == "/exit" {
                        return true;
                    }

                    if trimmed == "/stats" || trimmed == "/status" || trimmed == "/usage" {
                        self.force_refresh_models();
                        self.sections[self.selected]
                            .transcript
                            .push(format!("> {trimmed}"));
                        self.sections[self.selected]
                            .transcript
                            .push("< refreshing model quotas...".to_string());
                        self.input_buffer.clear();
                        self.input_mode = false;
                        return false;
                    }

                    if self.state.current_phase == Phase::IdeaInput {
                        self.input_buffer.clear();
                        self.input_mode = false;
                        self.launch_brainstorm(trimmed);
                        return false;
                    }

                    self.sections[self.selected]
                        .transcript
                        .push(format!("> {trimmed}"));
                    self.input_buffer.clear();
                }
                self.input_mode = false;
                false
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                false
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input_buffer.push(c);
                false
            }
            _ => false,
        }
    }

    fn toggle_selected_section(&mut self) {
        let current = self.current_section();
        if self.selected == current {
            return;
        }

        if self.sections[self.selected].status == super::state::SectionStatus::Pending {
            return;
        }

        if !self.expanded.insert(self.selected) {
            self.expanded.remove(&self.selected);
            self.transcript_open.remove(&self.selected);
        }
    }

    fn toggle_transcript(&mut self) {
        if !self.is_expanded(self.selected) || self.sections[self.selected].transcript.is_empty() {
            return;
        }

        if !self.transcript_open.insert(self.selected) {
            self.transcript_open.remove(&self.selected);
        }
    }

    fn scroll_selected(&mut self, delta: isize) {
        if !self.is_expanded(self.selected) {
            return;
        }

        let limit = self.selected_body_limit();
        let total = self.section_body(self.selected).len();
        let max_offset = total.saturating_sub(limit) as isize;
        let current = self.section_scroll_offset(self.selected, total, limit) as isize;
        let next = (current + delta).clamp(0, max_offset);
        self.section_scroll[self.selected] = next as usize;
    }

    pub(super) fn clamp_scroll(&mut self) {
        let limit = self.selected_body_limit();
        let total = self.section_body(self.selected).len();
        let max_offset = total.saturating_sub(limit);

        if self.section_scroll[self.selected] != usize::MAX {
            self.section_scroll[self.selected] = self.section_scroll[self.selected].min(max_offset);
        }
    }
}
