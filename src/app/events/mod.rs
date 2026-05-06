mod handlers;
mod input_focus;
mod interactive;
mod split;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app_runtime::{AppCommand, UiKey, UiKeyCode};
use crate::state::RunStatus;

use super::status_line::Severity;
use super::{App, ModalKind, PendingTermination, RetryLaunch, TerminationIntent};

impl App {
    fn marker_already_logged(&self, marker: &str) -> bool {
        let events_path = crate::state::session_dir(&self.state.session_id).join("events.toml");
        std::fs::read_to_string(&events_path).is_ok_and(|events| events.contains(marker))
    }

    fn request_termination(&mut self, pending: PendingTermination, _window_name: String) {
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
        self.runner_supervisor.cancel_run(pending.run_id);
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
    pub(crate) fn push_status(&self, message: String, severity: Severity, ttl: Duration) {
        self.status_line.borrow_mut().push(message, severity, ttl);
    }

    pub(crate) fn surface_boundary_error(&mut self, message: String, persist_agent_error: bool) {
        let _ = self.state.log_event(message.clone());
        self.push_status(message.clone(), Severity::Error, Duration::from_secs(8));
        if persist_agent_error {
            self.record_agent_error(message);
            let _ = self.state.save();
            self.rebuild_tree_view(None);
        }
    }

    pub(crate) fn stop_running_agent(&mut self) {
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

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
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

    pub(crate) fn handle_app_command(&mut self, command: AppCommand) -> bool {
        match command {
            AppCommand::KeyPress(key) => self.handle_key(key_event_from_ui_key(key)),
            AppCommand::PasteInput { text } => {
                self.handle_paste(&text);
                false
            }
            AppCommand::Quit => {
                if self.has_running_agent() {
                    self.open_quit_running_agent_modal();
                    false
                } else {
                    true
                }
            }
            AppCommand::OpenPalette => {
                self.open_palette_browser();
                false
            }
            AppCommand::StopAgent => {
                self.stop_running_agent();
                false
            }
            AppCommand::MoveFocus { delta } => {
                self.scroll_or_move_focus(delta);
                false
            }
            AppCommand::ToggleExpand => {
                self.toggle_expand_focused();
                false
            }
            AppCommand::OpenSplit => {
                if let Some(target) = self.resolve_split_target_for_selected_row() {
                    self.open_split_target(target);
                }
                false
            }
            AppCommand::CloseSplit => {
                self.close_split();
                false
            }
            AppCommand::SubmitInput { text } => {
                self.input_buffer = text;
                self.input_cursor = self.input_buffer.chars().count();
                self.handle_input_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            }
            AppCommand::CancelModal => {
                // Currently emitted only for the quit-confirmation modal; other
                // modals' Esc semantics still flow through `handle_modal_key`
                // via the legacy `KeyPress` bridge.
                self.pending_quit_confirmation_run_id = None;
                false
            }
            // The remaining command variants are exercised by the stubbed
            // runtime seam; legacy production handlers will claim them as the
            // surrounding modal/palette split moves out of `App`.
            _ => false,
        }
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
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
}

fn key_event_from_ui_key(key: UiKey) -> KeyEvent {
    let code = match key.code {
        UiKeyCode::Esc => KeyCode::Esc,
        UiKeyCode::Enter => KeyCode::Enter,
        UiKeyCode::Backspace => KeyCode::Backspace,
        UiKeyCode::Delete => KeyCode::Delete,
        UiKeyCode::Left => KeyCode::Left,
        UiKeyCode::Right => KeyCode::Right,
        UiKeyCode::Home => KeyCode::Home,
        UiKeyCode::End => KeyCode::End,
        UiKeyCode::Up => KeyCode::Up,
        UiKeyCode::Down => KeyCode::Down,
        UiKeyCode::PageUp => KeyCode::PageUp,
        UiKeyCode::PageDown => KeyCode::PageDown,
        UiKeyCode::Char(c) => KeyCode::Char(c),
        UiKeyCode::Unknown => KeyCode::Null,
    };
    let mut modifiers = KeyModifiers::NONE;
    if key.ctrl {
        modifiers |= KeyModifiers::CONTROL;
    }
    if key.alt {
        modifiers |= KeyModifiers::ALT;
    }
    KeyEvent::new(code, modifiers)
}
