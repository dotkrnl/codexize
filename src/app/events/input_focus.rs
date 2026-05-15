use super::super::{App, ExpansionOverride};
use crate::app_runtime::{UiKey, UiKeyCode};
use crate::state::{NodeStatus, Stage};

impl App {
    pub(crate) fn handle_input_key(&mut self, key: UiKey) -> bool {
        if self.interactive_run_active() && !self.interactive_run_waiting_for_input() {
            self.input_mode = false;
            return false;
        }
        if self.maybe_enter_command_mode_from_input_buffer() {
            return self.handle_palette_key(key);
        }
        match key.code {
            UiKeyCode::Esc => {
                self.input_mode = false;
                if self.is_split_open() {
                    self.close_split();
                }
                return false;
            }
            UiKeyCode::Enter => {
                let keep_input_open = self.interactive_run_waiting_for_input();
                let trimmed = self.input_buffer.trim().to_string();
                if !trimmed.is_empty() {
                    if trimmed == "/exit" && keep_input_open {
                        self.exit_interactive_run_locally();
                        self.input_buffer.clear();
                        self.input_cursor = 0;
                        self.input_mode = true;
                        return false;
                    }
                    if keep_input_open {
                        self.send_interactive_input(trimmed);
                        self.input_buffer.clear();
                        self.input_cursor = 0;
                        self.input_mode = true;
                        return false;
                    }
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
                    if self.state.current_stage == Stage::IdeaInput {
                        self.input_buffer.clear();
                        self.input_cursor = 0;
                        self.input_mode = false;
                        self.launch_brainstorm(trimmed);
                        return false;
                    }
                    self.input_buffer.clear();
                    self.input_cursor = 0;
                }
                self.input_mode = keep_input_open;
                return false;
            }
            _ => {}
        }
        let _ =
            crate::app::input_editor::apply(&mut self.input_buffer, &mut self.input_cursor, key);
        let _ = self.maybe_enter_command_mode_from_input_buffer();
        false
    }
    pub(crate) fn toggle_expand_focused(&mut self) {
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
    pub(crate) fn scroll_or_move_focus(&mut self, delta: isize) {
        let idx = self.selected;
        let area_h = self.effective_body_inner_height();
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
    pub(crate) fn move_focus(&mut self, delta: isize) {
        self.explicit_viewport_scroll = false;
        let before = self.selected;
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
        // Manual focus movement opts out of progress-follow until the next
        // stage transition or run launch resets the boundary. No-ops at the
        // top/bottom row do not actually change focus, so they leave the
        // follow flag alone.
        if self.selected != before {
            self.progress_follow_active = false;
        }
    }
}
