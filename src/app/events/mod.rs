use super::{App, AppStartupOrigin, ModalKind, status_line::Severity};
use crate::app_runtime::{AppCommand, UiKey, UiKeyCode};
use crate::state::RunStatus;
use std::time::Duration;

mod handlers;
mod input_focus;
mod interactive;
mod split;

impl App {
    pub(crate) fn marker_already_logged(&self, marker: &str) -> bool {
        let events_path = self.session_dir().join("events.toml");
        std::fs::read_to_string(&events_path).is_ok_and(|events| events.contains(marker))
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
        if let Err(e) = self.state.log_event(message.clone()) {
            tracing::warn!("failed to log boundary error event: {e}");
        }
        self.push_status(message.clone(), Severity::Error, Duration::from_secs(8));
        if persist_agent_error {
            self.record_agent_error(message);
            self.save_state();
            self.rebuild_tree_view(None);
        }
    }

    pub(crate) fn stop_running_agent(&mut self) {
        // Synchronize the FSM with the persisted state before invoking the
        // op. The FSM sync normally runs at run launch time
        // (start_run_tracking) and run finalization, but resume-path
        // sessions can hit `:stop` before any new run launches in this
        // process, which means the FSM is still Idle from construction.
        // A desync surfaces as `OpOutcome::NoOp("no agent running")` —
        // the operator sees the same status-line warning they'd see on a
        // genuinely idle session.
        self.sync_fsm_running_state();
        let outcome = self.with_lifecycle_ops_ctx(crate::lifecycle::LifecycleOps::stop);
        self.apply_op_outcome(outcome, "stop");
    }

    fn retry_running_agent(&mut self) {
        self.sync_fsm_running_state();
        let outcome = self.with_lifecycle_ops_ctx(crate::lifecycle::LifecycleOps::restart);
        self.apply_op_outcome(outcome, "retry");
    }

    pub(crate) fn start_command_available(&self) -> bool {
        !self.has_running_agent()
            && matches!(
                self.state.current_stage,
                crate::state::Stage::BrainstormRunning
                    | crate::state::Stage::SpecReviewRunning
                    | crate::state::Stage::PlanningRunning
                    | crate::state::Stage::PlanReviewRunning
                    | crate::state::Stage::WaitingToImplement
                    | crate::state::Stage::RepoStateUpdateRunning
                    | crate::state::Stage::ShardingRunning
                    | crate::state::Stage::ImplementationRound(_)
                    | crate::state::Stage::ReviewRound(_)
                    | crate::state::Stage::BuilderRecovery(_)
                    | crate::state::Stage::BuilderRecoveryPlanReview(_)
                    | crate::state::Stage::BuilderRecoverySharding(_)
                    | crate::state::Stage::Simplification(_)
                    | crate::state::Stage::FinalValidation(_)
                    | crate::state::Stage::Dreaming(_)
            )
    }

    pub(crate) fn start_agent_manually(&mut self) {
        if self.has_running_agent() {
            self.push_status(
                "Agent is already running.".to_string(),
                Severity::Info,
                Duration::from_secs(3),
            );
            return;
        }
        self.current_run_id = None;
        self.run_launched = false;
        if !matches!(self.fsm.view(), crate::lifecycle::AgentState::Idle) {
            self.fsm = crate::lifecycle::Fsm::new();
        }
        if !self.start_command_available() {
            self.push_status(
                "No startable agent for the current stage.".to_string(),
                Severity::Info,
                Duration::from_secs(3),
            );
            return;
        }
        if self.state.agent_error.is_some() {
            self.push_status(
                "Resolve or retry the stage error before starting.".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        }
        self.startup_origin = AppStartupOrigin::Default;
        if matches!(
            self.state.current_stage,
            crate::state::Stage::WaitingToImplement
        ) {
            self.dispatch_waiting_to_implement();
        }
        self.maybe_auto_launch();
        let status = if self.run_launched {
            "Starting agent..."
        } else if self.models.is_empty() {
            "Loading models before start..."
        } else {
            "No startable agent for the current stage."
        };
        self.push_status(status.to_string(), Severity::Info, Duration::from_secs(3));
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

    pub(crate) fn handle_key(&mut self, key: impl Into<UiKey>) -> bool {
        let key = key.into();
        if self.config_panel.is_some() {
            return self.handle_config_panel_key(key);
        }
        // Keep Ctrl+C global so palette/input/modal states cannot swallow an
        // operator stop, but preserve the historical quit path when idle.
        if key.code == UiKeyCode::Char('c') && key.ctrl {
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
            ) && key.code == UiKeyCode::Char(':')
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
                let text_entry_key = matches!(key.code, UiKeyCode::Enter)
                    || (matches!(key.code, UiKeyCode::Char(_)) && !key.ctrl && !key.alt);
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
        if self.confirm_back && key.code != UiKeyCode::Char(':') {
            self.confirm_back = false;
            self.status_line.borrow_mut().clear();
            return false;
        }
        if self.can_focus_input() && matches!(key.code, UiKeyCode::Char(_)) && !key.ctrl && !key.alt
        {
            self.input_mode = true;
            return self.handle_input_key(key);
        }
        match key.code {
            UiKeyCode::Esc => {
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
            UiKeyCode::Char('q' | 'Q') => {
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
            UiKeyCode::Char(':') => {
                self.open_palette_browser();
                false
            }
            UiKeyCode::Up => {
                self.scroll_or_move_focus(-1);
                false
            }
            UiKeyCode::Down => {
                self.scroll_or_move_focus(1);
                false
            }
            UiKeyCode::Char(' ') => {
                self.toggle_expand_focused();
                false
            }
            UiKeyCode::Enter => {
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
            UiKeyCode::PageUp => {
                let step = self.body_inner_height.saturating_sub(1).max(1) as isize;
                self.scroll_viewport(-step, true);
                false
            }
            UiKeyCode::PageDown => {
                let step = self.body_inner_height.saturating_sub(1).max(1) as isize;
                self.scroll_viewport(step, true);
                false
            }
            _ => false,
        }
    }

    pub(crate) fn open_config_panel_with_arg(&mut self, arg: &str) {
        if !crate::app::config_panel::can_open(self.body_inner_width as u16) {
            self.push_status(
                crate::app::config_panel::terminal_too_narrow_message().to_string(),
                Severity::Warn,
                Duration::from_secs(4),
            );
            return;
        }
        let initial = match arg.trim() {
            "" => self.last_config_section.as_deref(),
            non_empty => match crate::app::config_panel::lookup_section(non_empty) {
                crate::app::config_panel::SectionLookup::Exact(name)
                | crate::app::config_panel::SectionLookup::UniquePrefix(name) => Some(name),
                crate::app::config_panel::SectionLookup::Ambiguous(matches) => {
                    self.push_status(
                        format!("config: ambiguous section ({})", matches.join("|")),
                        Severity::Warn,
                        Duration::from_secs(4),
                    );
                    return;
                }
                crate::app::config_panel::SectionLookup::Unknown => {
                    self.push_status(
                        format!("config: unknown section \"{non_empty}\""),
                        Severity::Warn,
                        Duration::from_secs(4),
                    );
                    return;
                }
            },
        };
        let path = crate::data::config::paths::config_path();
        let config = crate::data::config::loader::load_from_path(&path)
            .unwrap_or_else(|_| (*self.config).clone());
        self.config_panel = Some(crate::app::config_panel::ConfigPanelState::open_at(
            &config, path, initial,
        ));
    }

    pub(crate) fn handle_config_panel_key(&mut self, key: UiKey) -> bool {
        let Some(panel) = self.config_panel.as_mut() else {
            return false;
        };
        let outcome = panel.handle_ui_key(key);
        match outcome {
            crate::app::config_panel::PanelOutcome::KeepOpen => false,
            crate::app::config_panel::PanelOutcome::Close => {
                self.remember_last_config_section();
                self.config_panel = None;
                false
            }
            crate::app::config_panel::PanelOutcome::Saved => {
                self.remember_last_config_section();
                self.reload_config_after_save();
                self.config_panel = None;
                self.push_status(
                    "saved · in effect immediately".to_string(),
                    Severity::Info,
                    Duration::from_secs(3),
                );
                false
            }
        }
    }

    fn remember_last_config_section(&mut self) {
        if let Some(panel) = self.config_panel.as_ref() {
            self.last_config_section = Some(panel.current_section_name().to_string());
        }
    }

    /// Re-read the unified config from disk and refresh the cached
    /// `Arc<Config>` plus its derived `view::*` snapshots so subsystems
    /// driven off `self.paths`, `self.memory_view`, and `self.ui_view`
    /// observe the new values without re-launching the App.
    pub(crate) fn reload_config_after_save(&mut self) {
        let path = crate::data::config::paths::config_path();
        match crate::data::config::loader::load_from_path(&path) {
            Ok(loaded) => {
                let arc = std::sync::Arc::new(loaded);
                self.paths = arc.paths_view();
                self.memory_view = arc.memory_view();
                self.ui_view = arc.ui_view();
                self.config = arc;
            }
            Err(err) => {
                self.push_status(
                    format!("config reload failed: {err}"),
                    Severity::Warn,
                    Duration::from_secs(4),
                );
            }
        }
    }

    pub(crate) fn handle_app_command(&mut self, command: AppCommand) -> bool {
        match command {
            AppCommand::KeyPress(key) => self.handle_key(key),
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
                self.handle_input_key(UiKey {
                    code: UiKeyCode::Enter,
                    ctrl: false,
                    alt: false,
                })
            }
            AppCommand::CancelModal => {
                // Currently emitted only for the quit-confirmation modal; other
                // modals' Esc semantics still flow through `handle_modal_key`
                // via the `KeyPress` bridge.
                self.pending_quit_confirmation_run_id = None;
                false
            }
            // The remaining command variants are exercised by the stubbed
            // runtime seam; production handlers claim them until the
            // surrounding modal/palette split moves out of `App`.
            _ => false,
        }
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        if self.palette.open {
            crate::app::input_editor::insert_str(
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
            crate::app::input_editor::insert_str(
                &mut self.palette.buffer,
                &mut self.palette.cursor,
                text,
            );
            return;
        }
        crate::app::input_editor::insert_str(&mut self.input_buffer, &mut self.input_cursor, text);
        let _ = self.maybe_enter_command_mode_from_input_buffer();
    }
}
