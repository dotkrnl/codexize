use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::state::{Phase, RunStatus};

use super::super::palette::{self, PaletteCommand};
use super::super::status_line::Severity;
use super::super::{App, ModalKind, PendingTermination, StageId, TerminationIntent};

impl App {
    pub(crate) fn palette_commands(&self) -> Vec<PaletteCommand> {
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

    pub(crate) fn handle_palette_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn execute_palette_input(&mut self, input: &str) -> bool {
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

    pub(crate) fn execute_palette_command(&mut self, name: &str, args: &str) -> bool {
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


    pub(crate) fn handle_modal_key(&mut self, modal: ModalKind, key: KeyEvent) -> bool {
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

    pub(crate) fn dismiss_interactive_exit_prompt(&mut self) {
        if let Some(key) = self.interactive_exit_prompt_key() {
            self.interactive_exit_prompt_dismissed_at = Some(key);
        }
    }

    pub(crate) fn handle_interactive_exit_prompt_modal_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn handle_quit_running_agent_modal_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn handle_spec_review_paused_modal_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn handle_plan_review_paused_modal_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn handle_stage_error_modal_key(&mut self, stage_id: StageId, key: KeyEvent) -> bool {
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


    pub(crate) fn handle_skip_to_impl_modal_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn handle_guard_modal_key(&mut self, key: KeyEvent) -> bool {
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

