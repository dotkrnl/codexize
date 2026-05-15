use super::super::App;
use super::super::CommandReturnTarget;
use super::super::split::SplitTarget;
use crate::state::Stage;
use crossterm::event::{KeyCode, KeyEvent};
impl App {
    /// Resolve the currently selected visible row to a split target, if any.
    ///
    /// Run rows (including collapsed parents that absorbed a leaf run id)
    /// map to `Run(id)`. The Idea node maps to `Idea`. Everything else
    /// returns `None`.
    pub(crate) fn resolve_split_target_for_selected_row(&self) -> Option<SplitTarget> {
        let node = self.node_for_row(self.selected)?;
        if let Some(run_id) = node.run_id.or(node.leaf_run_id) {
            return Some(SplitTarget::Run(run_id));
        }
        if node.label == "Idea" {
            return Some(SplitTarget::Idea);
        }
        None
    }
    /// Open the split for `target`. If the split is already showing the
    /// same target, this is a no-op (Enter must not toggle-close).
    /// Opening a different target resets scroll to the default tail position.
    pub(crate) fn open_split_target(&mut self, target: SplitTarget) {
        let same_target = self.split_target == Some(target);
        if same_target {
            return;
        }
        self.split_target = Some(target);
        self.split_scroll_offset = 0;
        self.split_follow_tail = true;
    }
    /// Close the split pane and return focus to the tree.
    pub(crate) fn close_split(&mut self) {
        self.split_target = None;
        self.split_scroll_offset = 0;
        self.split_follow_tail = true;
    }
    /// Returns `true` when the split is currently open.
    pub(crate) fn is_split_open(&self) -> bool {
        self.split_target.is_some()
    }
    pub(super) fn command_return_target_for_input_surface(&self) -> Option<CommandReturnTarget> {
        if self.state.current_stage == Stage::IdeaInput {
            return Some(CommandReturnTarget::Idea);
        }
        if self.interactive_run_waiting_for_input() {
            if self.split_owns_input() {
                return Some(CommandReturnTarget::SplitInteractive);
            }
            return Some(CommandReturnTarget::FooterInteractive);
        }
        None
    }
    pub(super) fn enter_command_mode_from_input_buffer(&mut self, target: CommandReturnTarget) {
        if !self.input_buffer.starts_with(':') {
            return;
        }
        let command_text: String = self.input_buffer.chars().skip(1).collect();
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.input_mode = false;
        self.palette.open_with_buffer(command_text);
        self.command_return_target = Some(target);
    }
    pub(super) fn maybe_enter_command_mode_from_input_buffer(&mut self) -> bool {
        let Some(target) = self.command_return_target_for_input_surface() else {
            return false;
        };
        if !self.input_buffer.starts_with(':') {
            return false;
        }
        self.enter_command_mode_from_input_buffer(target);
        true
    }
    pub(super) fn restore_input_focus_after_command_exit(&mut self) {
        let Some(target) = self.command_return_target.take() else {
            return;
        };
        match target {
            CommandReturnTarget::Idea => {
                if self.state.current_stage == Stage::IdeaInput {
                    self.input_mode = true;
                }
            }
            CommandReturnTarget::FooterInteractive => {
                self.input_mode =
                    self.interactive_run_waiting_for_input() && !self.split_owns_input();
            }
            CommandReturnTarget::SplitInteractive => {
                // If the split ownership changed while command mode was open,
                // refuse to force-focus the wrong surface.
                self.input_mode =
                    self.interactive_run_waiting_for_input() && self.split_owns_input();
            }
        }
    }
    pub(super) fn close_palette(&mut self, restore_input_focus: bool) {
        self.palette.close();
        if restore_input_focus {
            self.restore_input_focus_after_command_exit();
        } else {
            self.command_return_target = None;
        }
    }
    pub(super) fn open_palette_browser(&mut self) {
        self.palette.open();
        self.command_return_target = None;
    }
    pub(super) fn scroll_split_by_lines(&mut self, delta: isize) {
        let content_height = self.current_split_content_height();
        if content_height == 0 {
            self.split_scroll_offset = 0;
            self.split_follow_tail = true;
            return;
        }
        if delta < 0 {
            if self.split_follow_tail {
                self.split_follow_tail = false;
            }
            self.split_scroll_offset = self
                .split_scroll_offset
                .saturating_sub(delta.unsigned_abs());
        } else if delta > 0 {
            if self.split_follow_tail {
                return;
            }
            self.split_scroll_offset = self.split_scroll_offset.saturating_add(delta as usize);
        } else {
            return;
        }
        self.clamp_split_scroll(content_height);
    }
    pub(super) fn scroll_split_by_page(&mut self, delta: isize) {
        let step = self.split_viewport_height().saturating_sub(1).max(1) as isize;
        self.scroll_split_by_lines(delta.saturating_mul(step));
    }
    pub(super) fn handle_split_key(&mut self, key: KeyEvent) -> bool {
        if self.split_owns_input() {
            self.input_mode = true;
            match key.code {
                KeyCode::Esc | KeyCode::Enter => return self.handle_input_key(key),
                KeyCode::Up => {
                    self.scroll_split_by_lines(-1);
                    return false;
                }
                KeyCode::Down => {
                    self.scroll_split_by_lines(1);
                    return false;
                }
                KeyCode::PageUp => {
                    self.scroll_split_by_page(-1);
                    return false;
                }
                KeyCode::PageDown => {
                    self.scroll_split_by_page(1);
                    return false;
                }
                _ => {
                    self.handle_input_key(key);
                    return false;
                }
            }
        }
        match key.code {
            KeyCode::Esc => {
                self.close_split();
                false
            }
            KeyCode::Char(':') => {
                self.open_palette_browser();
                false
            }
            KeyCode::Up => {
                self.scroll_split_by_lines(-1);
                false
            }
            KeyCode::Down => {
                self.scroll_split_by_lines(1);
                false
            }
            KeyCode::PageUp => {
                self.scroll_split_by_page(-1);
                false
            }
            KeyCode::PageDown => {
                self.scroll_split_by_page(1);
                false
            }
            KeyCode::Enter
            | KeyCode::Char('q' | 'Q' | _)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End => false,
            _ => false,
        }
    }
}
