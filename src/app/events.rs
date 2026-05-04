use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::{Message, MessageKind, MessageSender, NodeStatus, Phase, RunStatus};

use super::palette::{self, PaletteCommand};
use super::split::SplitTarget;
use super::status_line::Severity;
use super::{
    App, CommandReturnTarget, ExpansionOverride, ModalKind, PendingTermination, RetryLaunch,
    StageId, TerminationIntent,
};

impl App {
    fn marker_already_logged(&self, marker: &str) -> bool {
        let events_path = crate::state::session_dir(&self.state.session_id).join("events.toml");
        std::fs::read_to_string(&events_path).is_ok_and(|events| events.contains(marker))
    }

    fn request_termination(&mut self, pending: PendingTermination, window_name: String) {
        if let Some(existing) = self.pending_termination.as_ref()
            && existing.run_id == pending.run_id
        {
            if existing.intent == pending.intent {
                return;
            }
            // Once cancellation has started, keep the first requested outcome so
            // repeated stop/retry/quit input cannot race contradictory follow-up work.
            self.push_status(
                format!(
                    "Termination already pending: keeping {}.",
                    existing.intent.summary()
                ),
                Severity::Warn,
                Duration::from_secs(5),
            );
            return;
        }

        let marker = format!("{}: run_id={}", pending.marker(), pending.run_id);
        if !self.marker_already_logged(&marker) {
            let _ = self.state.log_event(marker);
        }
        self.pending_quit_confirmation_run_id = None;
        self.pending_termination = Some(pending.clone());
        crate::app::prompts::cancel_run_label(&window_name);
        self.push_status(
            pending.intent.in_progress_status().to_string(),
            Severity::Warn,
            Duration::from_secs(5),
        );
    }

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

    pub(super) fn stop_running_agent(&mut self) {
        let Some(run) = self
            .running_run()
            .or_else(|| {
                self.state
                    .agent_runs
                    .iter()
                    .find(|r| r.status == RunStatus::Running)
            })
            .cloned()
        else {
            return;
        };

        self.request_termination(
            PendingTermination {
                run_id: run.id,
                intent: TerminationIntent::StopOnly,
            },
            run.window_name.clone(),
        );
    }

    fn retry_running_agent(&mut self) {
        let Some(run) = self
            .running_run()
            .or_else(|| {
                self.state
                    .agent_runs
                    .iter()
                    .find(|candidate| candidate.status == RunStatus::Running)
            })
            .cloned()
        else {
            return;
        };
        let Some(retry_launch) = RetryLaunch::for_run(&run) else {
            self.push_status(
                "retry: current run is not retryable".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        };

        self.request_termination(
            PendingTermination {
                run_id: run.id,
                intent: TerminationIntent::StopAndRetry(retry_launch),
            },
            run.window_name.clone(),
        );
    }

    fn open_quit_running_agent_modal(&mut self) {
        let running = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.status == RunStatus::Running)
            .map(|run| run.id)
            .collect::<Vec<_>>();
        if running.len() == 1 {
            self.pending_quit_confirmation_run_id = running.first().copied();
        }
    }

    /// Resolve the currently selected visible row to a split target, if any.
    ///
    /// Run rows (including collapsed parents that absorbed a leaf run id)
    /// map to `Run(id)`. The Idea node maps to `Idea`. Everything else
    /// returns `None`.
    pub(super) fn resolve_split_target_for_selected_row(&self) -> Option<SplitTarget> {
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
    pub(super) fn open_split_target(&mut self, target: SplitTarget) {
        let same_target = self.split_target == Some(target);
        if same_target {
            return;
        }
        self.split_target = Some(target);
        self.split_scroll_offset = 0;
        self.split_follow_tail = true;
    }

    /// Close the split pane and return focus to the tree.
    pub(super) fn close_split(&mut self) {
        self.split_target = None;
        self.split_scroll_offset = 0;
        self.split_follow_tail = true;
    }

    /// Returns `true` when the split is currently open.
    pub(super) fn is_split_open(&self) -> bool {
        self.split_target.is_some()
    }

    fn command_return_target_for_input_surface(&self) -> Option<CommandReturnTarget> {
        if self.state.current_phase == Phase::IdeaInput {
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

    fn enter_command_mode_from_input_buffer(&mut self, target: CommandReturnTarget) {
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

    fn maybe_enter_command_mode_from_input_buffer(&mut self) -> bool {
        let Some(target) = self.command_return_target_for_input_surface() else {
            return false;
        };
        if !self.input_buffer.starts_with(':') {
            return false;
        }
        self.enter_command_mode_from_input_buffer(target);
        true
    }

    fn restore_input_focus_after_command_exit(&mut self) {
        let Some(target) = self.command_return_target.take() else {
            return;
        };
        match target {
            CommandReturnTarget::Idea => {
                if self.state.current_phase == Phase::IdeaInput {
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

    fn close_palette(&mut self, restore_input_focus: bool) {
        self.palette.close();
        if restore_input_focus {
            self.restore_input_focus_after_command_exit();
        } else {
            self.command_return_target = None;
        }
    }

    fn open_palette_browser(&mut self) {
        self.palette.open();
        self.command_return_target = None;
    }

    fn scroll_split_by_lines(&mut self, delta: isize) {
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

    fn scroll_split_by_page(&mut self, delta: isize) {
        let step = self.split_viewport_height().saturating_sub(1).max(1) as isize;
        self.scroll_split_by_lines(delta.saturating_mul(step));
    }

    fn handle_split_key(&mut self, key: KeyEvent) -> bool {
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
            KeyCode::Char('q') | KeyCode::Char('Q') => false,
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
            | KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End => false,
            _ => false,
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        // Keep Ctrl+C global so palette/input/modal states cannot swallow an
        // operator stop, but preserve the historical quit path when idle.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.has_running_agent() {
                self.stop_running_agent();
                return false;
            }
            return true;
        }

        if self.palette.open {
            return self.handle_palette_key(key);
        }

        if let Some(modal) = self.active_modal() {
            if matches!(
                modal,
                ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused
            ) && key.code == KeyCode::Char(':')
            {
                // Approval pauses render as modals, but YOLO must be toggleable
                // while paused so the gate can resolve on the next loop tick.
                self.open_palette_browser();
                return false;
            }
            self.confirm_back = false;
            return self.handle_modal_key(modal, key);
        }

        if self.is_split_open() {
            return self.handle_split_key(key);
        }

        if self.interactive_run_active() {
            if self.interactive_run_waiting_for_input() {
                let text_entry_key = matches!(key.code, KeyCode::Enter)
                    || (matches!(key.code, KeyCode::Char(_))
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT));
                if self.input_mode || text_entry_key {
                    self.input_mode = true;
                    return self.handle_input_key(key);
                }
            } else {
                self.input_mode = false;
            }
        }

        if self.input_mode {
            return self.handle_input_key(key);
        }

        if self.confirm_back && key.code != KeyCode::Char(':') {
            self.confirm_back = false;
            self.status_line.borrow_mut().clear();
            return false;
        }

        if self.can_focus_input()
            && matches!(key.code, KeyCode::Char(_))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.input_mode = true;
            return self.handle_input_key(key);
        }

        match key.code {
            KeyCode::Esc => {
                if self.is_split_open() {
                    self.close_split();
                    return false;
                }
                if self.has_running_agent() {
                    self.push_status(
                        "agent running — use palette commands".to_string(),
                        Severity::Warn,
                        Duration::from_secs(3),
                    );
                    false
                } else {
                    true
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                if self.has_running_agent() {
                    self.push_status(
                        "agent running — use palette commands".to_string(),
                        Severity::Warn,
                        Duration::from_secs(3),
                    );
                    false
                } else {
                    true
                }
            }
            KeyCode::Char(':') => {
                self.open_palette_browser();
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
                if !self.has_running_agent() && self.selected_retry_target().is_some() {
                    self.retry_selected_target();
                } else if let Some(target) = self.resolve_split_target_for_selected_row() {
                    self.open_split_target(target);
                }
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

    pub(super) fn handle_paste(&mut self, text: &str) {
        if self.palette.open {
            crate::input_editor::insert_str(
                &mut self.palette.buffer,
                &mut self.palette.cursor,
                text,
            );
            return;
        }

        if self.interactive_run_active() && !self.interactive_run_waiting_for_input() {
            self.input_mode = false;
            return;
        }

        let can_edit_input = self.split_owns_input()
            || self.interactive_run_waiting_for_input()
            || self.input_mode
            || self.can_focus_input();
        if !can_edit_input {
            return;
        }
        self.input_mode = true;

        if self.maybe_enter_command_mode_from_input_buffer() {
            crate::input_editor::insert_str(
                &mut self.palette.buffer,
                &mut self.palette.cursor,
                text,
            );
            return;
        }

        crate::input_editor::insert_str(&mut self.input_buffer, &mut self.input_cursor, text);
        let _ = self.maybe_enter_command_mode_from_input_buffer();
    }

    pub(super) fn palette_commands(&self) -> Vec<PaletteCommand> {
        // Direct keys in the running app (see `handle_key`): `Esc` quits the
        // TUI when no agent is running, while Ctrl-C stops a running agent.
        // `:` opens the palette. Everything else
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
            PaletteCommand {
                name: "stop",
                aliases: &[],
                help: "Stop the running agent without retry",
                key_hint: Some("Ctrl-C"),
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
        if self.has_running_agent() || self.selected_retry_target().is_some() {
            commands.push(PaletteCommand {
                name: "retry",
                aliases: &["r"],
                help: if self.has_running_agent() {
                    "Stop and retry the running agent"
                } else {
                    "Retry selected stage or task"
                },
                key_hint: None,
            });
        }
        if self.interactive_run_active() {
            commands.push(PaletteCommand {
                name: "interrupt",
                aliases: &[],
                help: "Interrupt ACP turn and send a new prompt",
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
            KeyCode::Esc => {
                self.close_palette(true);
                false
            }
            KeyCode::Enter => {
                let input = self.palette.buffer.clone();
                self.close_palette(false);
                self.execute_palette_input(&input)
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
                    self.close_palette(true);
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
            "quit" => {
                if self.has_running_agent() {
                    self.open_quit_running_agent_modal();
                    false
                } else {
                    true
                }
            }
            "back" => {
                self.confirm_back = false;
                self.status_line.borrow_mut().clear();
                if self.can_go_back() {
                    self.go_back();
                }
                false
            }
            "retry" => {
                if self.has_running_agent() {
                    self.retry_running_agent();
                } else {
                    self.retry_selected_target();
                }
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
            "stop" => {
                if self.has_running_agent() {
                    self.stop_running_agent();
                } else {
                    self.push_status(
                        "No active agent run to stop.".to_string(),
                        Severity::Info,
                        Duration::from_secs(3),
                    );
                }
                false
            }
            "interrupt" => {
                self.interrupt_interactive_input(args.trim().to_string());
                false
            }
            _ => false,
        }
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
            // not agent prompt text, so the runner completes this run by label.
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

    fn append_user_input_message(&mut self, run_id: u64, input: String) {
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
    }

    fn interrupt_interactive_input(&mut self, input: String) {
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            self.push_status(
                "interrupt requires a message".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        }
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
        if crate::runner::interrupt_run_label_input(&run.window_name, trimmed.clone()) {
            self.append_user_input_message(run_id, trimmed);
        } else {
            self.push_status(
                "interactive agent is not running".to_string(),
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
            ModalKind::QuitRunningAgent => self.handle_quit_running_agent_modal_key(key),
            ModalKind::InteractiveExitPrompt => self.handle_interactive_exit_prompt_modal_key(key),
            ModalKind::SpecReviewPaused => self.handle_spec_review_paused_modal_key(key),
            ModalKind::PlanReviewPaused => self.handle_plan_review_paused_modal_key(key),
            ModalKind::StageError(stage_id) => self.handle_stage_error_modal_key(stage_id, key),
        }
    }

    fn dismiss_interactive_exit_prompt(&mut self) {
        if let Some(key) = self.interactive_exit_prompt_key() {
            self.interactive_exit_prompt_dismissed_at = Some(key);
        }
    }

    fn handle_interactive_exit_prompt_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter => {
                self.interactive_exit_prompt_dismissed_at = None;
                self.exit_interactive_run_locally();
                false
            }
            KeyCode::Esc => {
                self.dismiss_interactive_exit_prompt();
                self.input_mode = true;
                false
            }
            KeyCode::Char(_)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.dismiss_interactive_exit_prompt();
                self.input_mode = true;
                self.handle_input_key(key)
            }
            _ => false,
        }
    }

    fn handle_quit_running_agent_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                let Some(run_id) = self.pending_quit_confirmation_run_id.take() else {
                    return false;
                };
                let Some(run) = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|candidate| {
                        candidate.id == run_id && candidate.status == RunStatus::Running
                    })
                    .cloned()
                else {
                    return false;
                };
                self.request_termination(
                    PendingTermination {
                        run_id,
                        intent: TerminationIntent::StopAndQuit,
                    },
                    run.window_name.clone(),
                );
                false
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.pending_quit_confirmation_run_id = None;
                false
            }
            _ => false,
        }
    }

    fn handle_spec_review_paused_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
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

        if self.maybe_enter_command_mode_from_input_buffer() {
            return self.handle_palette_key(key);
        }

        match key.code {
            KeyCode::Esc => {
                self.input_mode = false;
                if self.is_split_open() {
                    self.close_split();
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
        let _ = self.maybe_enter_command_mode_from_input_buffer();
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
