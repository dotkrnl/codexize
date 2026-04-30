use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::{Message, MessageKind, MessageSender, NodeStatus, Phase, RunStatus};

use super::palette::{self, PaletteCommand};
use super::status_line::Severity;
use super::{App, ExpansionOverride, ModalKind, StageId};

impl App {
    /// Push a transient status-line message from a non-render call site.
    ///
    /// Single entry point so renderer toasts and side-effect producers
    /// (refresh worker, guard logic, key handlers) share the same
    /// severity-priority + TTL contract enforced by `StatusLine`.
    pub(super) fn push_status(&self, message: String, severity: Severity, ttl: Duration) {
        self.status_line.borrow_mut().push(message, severity, ttl);
    }

    pub(super) fn surface_boundary_error(&mut self, message: String, persist_agent_error: bool) {
        let _ = self.state.log_event(message.clone());
        self.push_status(message.clone(), Severity::Error, Duration::from_secs(8));
        if persist_agent_error {
            self.record_agent_error(message);
            let _ = self.state.save();
            self.rebuild_tree_view(None);
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        if self.palette.open {
            return self.handle_palette_key(key);
        }

        if self.interactive_run_active() {
            if key.code == KeyCode::Char(':') {
                self.palette.open();
                return false;
            }
            if self.interactive_run_waiting_for_input() {
                self.input_mode = true;
                return self.handle_input_key(key);
            }
            self.input_mode = false;
            return false;
        }

        if self.input_mode {
            return self.handle_input_key(key);
        }

        if let Some(modal) = self.active_modal() {
            if matches!(
                modal,
                ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused
            ) && key.code == KeyCode::Char(':')
            {
                // Approval pauses render as modals, but YOLO must be toggleable
                // while paused so the gate can resolve on the next loop tick.
                self.palette.open();
                return false;
            }
            self.confirm_back = false;
            return self.handle_modal_key(modal, key);
        }

        if self.confirm_back && key.code != KeyCode::Char(':') {
            self.confirm_back = false;
            self.status_line.borrow_mut().clear();
            return false;
        }

        if self.can_focus_input()
            && matches!(key.code, KeyCode::Char(c) if c != ':')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.input_mode = true;
            return self.handle_input_key(key);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                if self.has_running_agent() {
                    self.push_status(
                        "agent running — use :quit to exit".to_string(),
                        Severity::Warn,
                        Duration::from_secs(3),
                    );
                    false
                } else {
                    true
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char(':') => {
                self.palette.open();
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
                if self.can_focus_input() {
                    self.input_mode = true;
                }
                false
            }
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

    pub(super) fn palette_commands(&self) -> Vec<PaletteCommand> {
        // Direct keys in the running app (see `handle_key`): only `Esc` and
        // Ctrl-C quit the TUI, plus `:` opens the palette. Everything else
        // is palette-only, so `back`, `edit`, `cheap`, and `yolo` advertise
        // no shortcut even though they have palette aliases.
        let mut commands = vec![
            PaletteCommand {
                name: "quit",
                aliases: &["q"],
                help: "Exit the TUI",
                key_hint: Some("Esc"),
            },
            PaletteCommand {
                name: "cheap",
                aliases: &[],
                help: "Toggle cheap mode",
                key_hint: None,
            },
            PaletteCommand {
                name: "yolo",
                aliases: &[],
                help: "Toggle YOLO mode",
                key_hint: None,
            },
            PaletteCommand {
                name: "texts",
                aliases: &["text", "messages"],
                help: "Toggle non-interactive agent text",
                key_hint: None,
            },
            PaletteCommand {
                name: "verbose",
                aliases: &["thinking", "thoughts"],
                help: "Toggle thinking text",
                key_hint: None,
            },
        ];
        if self.can_go_back() || self.confirm_back {
            commands.push(PaletteCommand {
                name: "back",
                aliases: &["b"],
                help: "Go back",
                key_hint: None,
            });
        }
        if self.selected_retry_target().is_some() {
            commands.push(PaletteCommand {
                name: "retry",
                aliases: &["r"],
                help: "Retry selected stage or task",
                key_hint: None,
            });
        }
        if self.editable_artifact().is_some() {
            commands.push(PaletteCommand {
                name: "edit",
                aliases: &["e"],
                help: "Edit artifact",
                key_hint: None,
            });
        }
        commands
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                if self.interactive_run_active() && key.code == KeyCode::Esc {
                    self.palette.buffer.clear();
                    self.palette.cursor = 0;
                    false
                } else if !self.interactive_run_active() {
                    self.palette.close();
                    false
                } else {
                    self.insert_palette_char(match key.code {
                        KeyCode::Char(c) => c,
                        _ => return false,
                    })
                }
            }
            KeyCode::Enter => {
                let input = self.palette.buffer.clone();
                if self.interactive_run_active() {
                    self.palette.buffer.clear();
                    self.palette.cursor = 0;
                } else {
                    self.palette.close();
                }
                let should_quit = self.execute_palette_input(&input);
                if self.interactive_run_active() {
                    self.palette.open();
                }
                should_quit
            }
            KeyCode::Tab => {
                let commands = self.palette_commands();
                if let Some(ghost) = palette::ghost_completion(&self.palette.buffer, &commands) {
                    self.palette.accept_ghost(ghost);
                }
                false
            }
            KeyCode::Backspace => {
                if self.palette.buffer.is_empty() {
                    self.palette.close();
                } else {
                    let cursor = self.palette.cursor;
                    if cursor > 0 {
                        let byte = self
                            .palette
                            .buffer
                            .char_indices()
                            .nth(cursor - 1)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let end = self
                            .palette
                            .buffer
                            .char_indices()
                            .nth(cursor)
                            .map(|(i, _)| i)
                            .unwrap_or(self.palette.buffer.len());
                        self.palette.buffer.replace_range(byte..end, "");
                        self.palette.cursor -= 1;
                    }
                }
                false
            }
            KeyCode::Char(c) => {
                let byte = self
                    .palette
                    .buffer
                    .char_indices()
                    .nth(self.palette.cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(self.palette.buffer.len());
                self.palette.buffer.insert(byte, c);
                self.palette.cursor += 1;
                false
            }
            _ => false,
        }
    }

    fn execute_palette_input(&mut self, input: &str) -> bool {
        if input.trim() == "/exit" && self.interactive_run_active() {
            self.exit_interactive_run_locally();
            return false;
        }

        let commands = self.palette_commands();
        match palette::resolve(input, &commands) {
            palette::MatchResult::Exact { command, args }
            | palette::MatchResult::UniquePrefix { command, args } => {
                if self.interactive_run_active() && command.name == "quit" {
                    if self.interactive_run_waiting_for_input() {
                        self.send_interactive_input(input.trim().to_string());
                    } else {
                        self.push_status(
                            "interactive agent is not ready for input".to_string(),
                            Severity::Warn,
                            Duration::from_secs(3),
                        );
                    }
                    return false;
                }
                let _ = self.state.log_event(format!(
                    "palette_invoked: command={} args={args}",
                    command.name
                ));
                self.execute_palette_command(command.name, &args)
            }
            palette::MatchResult::Ambiguous { candidates, .. } => {
                let names = candidates.join("|");
                self.push_status(
                    format!("palette: ambiguous ({names})"),
                    Severity::Warn,
                    Duration::from_secs(3),
                );
                false
            }
            palette::MatchResult::Unknown { input: cmd } => {
                if self.interactive_run_waiting_for_input() {
                    self.send_interactive_input(cmd);
                    return false;
                }
                self.push_status(
                    format!("palette: unknown command \"{cmd}\""),
                    Severity::Warn,
                    Duration::from_secs(3),
                );
                false
            }
        }
    }

    fn execute_palette_command(&mut self, name: &str, args: &str) -> bool {
        match name {
            "quit" => true,
            "back" => {
                self.confirm_back = false;
                self.status_line.borrow_mut().clear();
                if self.can_go_back() {
                    self.go_back();
                }
                false
            }
            "retry" => {
                self.retry_selected_target();
                false
            }
            "edit" => {
                self.open_editable_artifact();
                false
            }
            "cheap" => {
                match args.trim() {
                    "on" => self.set_cheap_mode(true, "palette"),
                    "off" => self.set_cheap_mode(false, "palette"),
                    _ => self.toggle_cheap_mode("palette"),
                }
                false
            }
            "yolo" => {
                match args.trim() {
                    "on" => self.set_yolo_mode(true, "palette"),
                    "off" => self.set_yolo_mode(false, "palette"),
                    _ => self.toggle_yolo_mode("palette"),
                }
                false
            }
            "texts" => {
                self.toggle_noninteractive_texts();
                false
            }
            "verbose" => {
                self.toggle_thinking_texts();
                false
            }
            _ => false,
        }
    }

    fn insert_palette_char(&mut self, c: char) -> bool {
        let byte = self
            .palette
            .buffer
            .char_indices()
            .nth(self.palette.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.palette.buffer.len());
        self.palette.buffer.insert(byte, c);
        self.palette.cursor += 1;
        false
    }

    pub(super) fn interactive_run_active(&self) -> bool {
        let Some(run_id) = self.current_run_id else {
            return false;
        };
        self.state.agent_runs.iter().any(|run| {
            run.id == run_id && run.status == RunStatus::Running && run.modes.interactive
        })
    }

    pub(super) fn interactive_run_waiting_for_input(&self) -> bool {
        let Some(run_id) = self.current_run_id else {
            return false;
        };
        self.state.agent_runs.iter().any(|run| {
            run.id == run_id
                && run.status == RunStatus::Running
                && run.modes.interactive
                && crate::runner::run_label_is_waiting_for_input(&run.window_name)
        })
    }

    fn exit_interactive_run_locally(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        if let Some(run) = self.state.agent_runs.iter().find(|run| run.id == run_id) {
            // `/exit` is a local codexize control for interactive ACP runs,
            // not agent prompt text, so the runner is cancelled by run label.
            crate::runner::request_run_label_exit(&run.window_name);
        }
    }

    fn send_interactive_input(&mut self, input: String) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            return;
        };
        if crate::runner::send_run_label_input(&run.window_name, input.clone()) {
            let message = Message {
                ts: chrono::Utc::now(),
                run_id,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: input,
            };
            if let Err(err) = self.state.append_message(&message) {
                let _ = self.state.log_event(format!(
                    "failed to append user input for run {run_id}: {err}"
                ));
            } else {
                self.messages.push(message);
                self.agent_last_change = Some(std::time::Instant::now());
            }
        } else {
            self.push_status(
                "interactive agent is not ready for input".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
        }
    }

    fn toggle_noninteractive_texts(&mut self) {
        self.state.show_noninteractive_texts = !self.state.show_noninteractive_texts;
        let label = if self.state.show_noninteractive_texts {
            "showing non-interactive agent text"
        } else {
            "hiding non-interactive agent text"
        };
        let _ = self.state.log_event(format!(
            "show_noninteractive_texts={}",
            self.state.show_noninteractive_texts
        ));
        if let Err(err) = self.state.save() {
            self.push_status(
                format!("texts: failed to save setting: {err}"),
                Severity::Error,
                Duration::from_secs(5),
            );
        } else {
            self.push_status(label.to_string(), Severity::Info, Duration::from_secs(3));
        }
    }

    fn toggle_thinking_texts(&mut self) {
        self.state.show_thinking_texts = !self.state.show_thinking_texts;
        let label = if self.state.show_thinking_texts {
            "showing thinking text"
        } else {
            "hiding thinking text"
        };
        let _ = self.state.log_event(format!(
            "show_thinking_texts={}",
            self.state.show_thinking_texts
        ));
        if let Err(err) = self.state.save() {
            self.push_status(
                format!("verbose: failed to save setting: {err}"),
                Severity::Error,
                Duration::from_secs(5),
            );
            return;
        }
        self.push_status(label.to_string(), Severity::Info, Duration::from_secs(3));
    }

    fn handle_modal_key(&mut self, modal: ModalKind, key: KeyEvent) -> bool {
        match modal {
            ModalKind::SkipToImpl => self.handle_skip_to_impl_modal_key(key),
            ModalKind::GitGuard => self.handle_guard_modal_key(key),
            ModalKind::SpecReviewPaused => self.handle_spec_review_paused_modal_key(key),
            ModalKind::PlanReviewPaused => self.handle_plan_review_paused_modal_key(key),
            ModalKind::StageError(stage_id) => self.handle_stage_error_modal_key(stage_id, key),
        }
    }

    fn handle_spec_review_paused_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('y') | KeyCode::Enter => {
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::PlanningRunning);
                false
            }
            KeyCode::Char('n') => {
                let _ = self.transition_to_phase(Phase::SpecReviewRunning);
                self.launch_spec_review();
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }

    fn handle_plan_review_paused_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('y') | KeyCode::Enter => {
                self.clear_agent_error();
                self.queue_view_of_current_artifact("plan.md");
                let _ = self.transition_to_phase(Phase::ShardingRunning);
                false
            }
            KeyCode::Char('n') => {
                let _ = self.transition_to_phase(Phase::PlanReviewRunning);
                self.launch_plan_review();
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }

    fn handle_stage_error_modal_key(&mut self, stage_id: StageId, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('r') | KeyCode::Enter => {
                match stage_id {
                    StageId::Brainstorm => {
                        let idea = self.state.idea_text.clone().unwrap_or_default();
                        self.launch_brainstorm(idea);
                    }
                    StageId::SpecReview => self.launch_spec_review(),
                    StageId::Planning => self.launch_planning(),
                    StageId::PlanReview => self.launch_plan_review(),
                    StageId::Sharding => self.launch_sharding(),
                    StageId::Implementation => self.launch_coder(),
                    StageId::Review => self.launch_reviewer(),
                }
                false
            }
            KeyCode::Char('e') if stage_id == StageId::Brainstorm => {
                let _ = self.transition_to_phase(Phase::IdeaInput);
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> bool {
        if self.interactive_run_active() && !self.interactive_run_waiting_for_input() {
            self.input_mode = false;
            return false;
        }

        match key.code {
            KeyCode::Esc => {
                if !self.interactive_run_waiting_for_input() {
                    self.input_mode = false;
                }
                return false;
            }
            KeyCode::Enter => {
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
                self.input_mode = keep_input_open;
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

    fn move_focus(&mut self, delta: isize) {
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
        // phase transition or run launch resets the boundary. No-ops at the
        // top/bottom row do not actually change focus, so they leave the
        // follow flag alone.
        if self.selected != before {
            self.progress_follow_active = false;
        }
    }

    fn handle_skip_to_impl_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if let Err(err) = self.accept_skip_to_implementation() {
                    self.record_agent_error(format!(
                        "accept skip-to-implementation failed: {err:#}"
                    ));
                }
                false
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                if let Err(err) = self.decline_skip_to_implementation() {
                    self.record_agent_error(format!(
                        "decline skip-to-implementation failed: {err:#}"
                    ));
                }
                false
            }
            _ => false,
        }
    }

    fn handle_guard_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                if let Err(err) = self.accept_guard_reset() {
                    self.record_agent_error(format!("guard reset failed: {err:#}"));
                }
                false
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                if let Err(err) = self.accept_guard_keep() {
                    self.record_agent_error(format!("guard keep failed: {err:#}"));
                }
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }
}
