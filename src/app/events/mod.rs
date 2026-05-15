use super::{App, AppStartupOrigin, ModalKind, status_line::Severity};
use crate::app_runtime::{
    AppCommand, ConfigPanelCommand, GlobalCommand, InputCommand, ModalAction, ModalCommand,
    ModesCommand, PaletteCommand, SessionCommand, SplitCommand, StageCommand, StatusCommand,
    TreeCommand,
};
use crate::state::RunStatus;
use std::time::Duration;

use crate::app::keys::{UiKey, UiKeyCode};

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

    #[cfg(test)]
    pub(crate) fn handle_key(&mut self, key: impl Into<UiKey>) -> bool {
        let key = key.into();
        if self.config_panel.is_some() {
            let cmd = crate::input_key::config_panel_key_to_command(key);
            return self.handle_config_panel_command(cmd);
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

    // NOTE: `handle_config_panel_key` deleted — config-panel keys are now
    // translated to `ConfigPanelCommand` before crossing the seam.  The
    // typed entry point is `handle_config_panel_command` below.

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

    /// Dispatch a typed seam-level [`AppCommand`] into the focus-local
    /// handlers. Each variant translates into one or more typed sub-command
    /// calls; the runtime no longer matches on raw `UiKey`s here.
    pub(crate) fn handle_app_command(&mut self, command: AppCommand) -> bool {
        match command {
            AppCommand::Global(cmd) => self.handle_global_command(cmd),
            AppCommand::Shell(_) => {
                // Shell-level commands are intercepted by `AppShell` before
                // they reach the focused `App`. Reaching here is a no-op.
                false
            }
            AppCommand::Session(_session_id, cmd) => self.handle_session_command(cmd),
        }
    }

    fn handle_global_command(&mut self, cmd: GlobalCommand) -> bool {
        match cmd {
            GlobalCommand::Quit => {
                if self.has_running_agent() {
                    self.open_quit_running_agent_modal();
                    false
                } else {
                    true
                }
            }
            GlobalCommand::StopRunningAgent => {
                if self.has_running_agent() {
                    self.stop_running_agent();
                    false
                } else {
                    // Historic Ctrl-C behavior: when no agent is running,
                    // Ctrl-C quits the TUI.
                    true
                }
            }
        }
    }

    fn handle_session_command(&mut self, cmd: SessionCommand) -> bool {
        match cmd {
            SessionCommand::Tree(c) => self.handle_tree_command(c),
            SessionCommand::Palette(c) => self.handle_palette_command(c),
            SessionCommand::Input(c) => self.handle_input_command(c),
            SessionCommand::Modal(c) => self.handle_modal_command_dispatch(c),
            SessionCommand::Stage(c) => self.handle_stage_command(c),
            SessionCommand::Modes(c) => self.handle_modes_command(c),
            SessionCommand::Split(c) => self.handle_split_command(c),
            SessionCommand::ConfigPanel(c) => self.handle_config_panel_command(c),
            SessionCommand::Status(c) => match c {
                StatusCommand::Dismiss => {
                    self.status_line.borrow_mut().clear();
                    false
                }
            },
            SessionCommand::Chat(_) | SessionCommand::Picker(_) | SessionCommand::Sheet(_) => false,
            SessionCommand::SubmitInput { text } => {
                self.input_buffer = text;
                self.input_cursor = self.input_buffer.chars().count();
                self.handle_input_key(UiKey {
                    code: UiKeyCode::Enter,
                    ctrl: false,
                    alt: false,
                })
            }
            SessionCommand::PaletteCommand { name, args } => {
                self.execute_palette_command(&name, &args)
            }
        }
    }

    fn handle_tree_command(&mut self, cmd: TreeCommand) -> bool {
        match cmd {
            TreeCommand::ScrollOrMoveFocus { delta } => {
                self.scroll_or_move_focus(delta);
                false
            }
            TreeCommand::MoveFocus { delta } => {
                self.move_focus(delta);
                false
            }
            TreeCommand::ScrollViewportPage { delta } => {
                let step = self.body_inner_height.saturating_sub(1).max(1) as isize;
                self.scroll_viewport(delta.saturating_mul(step), true);
                false
            }
            TreeCommand::ToggleExpand => {
                self.toggle_expand_focused();
                false
            }
            TreeCommand::ActivateFocused => {
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
        }
    }

    fn handle_palette_command(&mut self, cmd: PaletteCommand) -> bool {
        match cmd {
            PaletteCommand::Open => {
                self.open_palette_browser();
                false
            }
            PaletteCommand::Close {
                restore_input_focus,
            } => {
                self.close_palette(restore_input_focus);
                false
            }
            PaletteCommand::Submit => {
                let input = self.palette.buffer.clone();
                self.close_palette(false);
                self.execute_palette_input(&input)
            }
            PaletteCommand::AcceptGhost => {
                if !self.palette.open {
                    return false;
                }
                let commands = self.palette_commands();
                if let Some(ghost) =
                    crate::app::palette::ghost_completion(&self.palette.buffer, &commands)
                {
                    self.palette.accept_ghost(ghost);
                }
                false
            }
            PaletteCommand::Edit(input_cmd) => {
                // Backspace on an empty palette buffer closes the palette
                // (matches the legacy key handler behavior).
                if matches!(input_cmd, InputCommand::Backspace) && self.palette.buffer.is_empty() {
                    self.close_palette(true);
                    return false;
                }
                crate::app::input_editor::apply_input_command(
                    &mut self.palette.buffer,
                    &mut self.palette.cursor,
                    &input_cmd,
                );
                false
            }
        }
    }

    fn handle_input_command(&mut self, cmd: InputCommand) -> bool {
        match cmd {
            InputCommand::Submit => self.handle_input_key(UiKey {
                code: UiKeyCode::Enter,
                ctrl: false,
                alt: false,
            }),
            InputCommand::Cancel => self.handle_input_key(UiKey {
                code: UiKeyCode::Esc,
                ctrl: false,
                alt: false,
            }),
            InputCommand::InsertText(text) => {
                if !self.input_mode && self.can_focus_input() {
                    self.input_mode = true;
                }
                if self.maybe_enter_command_mode_from_input_buffer() {
                    crate::app::input_editor::insert_str(
                        &mut self.palette.buffer,
                        &mut self.palette.cursor,
                        &text,
                    );
                    return false;
                }
                crate::app::input_editor::insert_str(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    &text,
                );
                let _ = self.maybe_enter_command_mode_from_input_buffer();
                false
            }
            InputCommand::ReplaceBuffer(text) => {
                self.input_buffer = text;
                self.input_cursor = self.input_buffer.chars().count();
                false
            }
            other => {
                crate::app::input_editor::apply_input_command(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    &other,
                );
                let _ = self.maybe_enter_command_mode_from_input_buffer();
                false
            }
        }
    }

    fn handle_modal_command_dispatch(&mut self, cmd: ModalCommand) -> bool {
        let Some(modal) = self.active_modal() else {
            // Quit-confirmation lives outside `active_modal()` for legacy
            // reasons; route a bare Cancel to clear its pending state.
            if matches!(cmd, ModalCommand::Cancel) {
                self.pending_quit_confirmation_run_id = None;
            }
            return false;
        };
        match modal {
            ModalKind::QuitRunningAgent => match cmd {
                ModalCommand::Confirm => {
                    if self.pending_quit_confirmation_run_id.take().is_none() {
                        return false;
                    }
                    self.run_lifecycle_op("cancel", crate::lifecycle::LifecycleOps::cancel);
                    self.pending_app_exit = true;
                    false
                }
                ModalCommand::Cancel => {
                    self.pending_quit_confirmation_run_id = None;
                    false
                }
                _ => false,
            },
            ModalKind::CancelSession => match cmd {
                ModalCommand::Confirm => self.handle_cancel_session_modal_key(UiKey {
                    code: UiKeyCode::Enter,
                    ctrl: false,
                    alt: false,
                }),
                ModalCommand::Cancel => self.handle_cancel_session_modal_key(UiKey {
                    code: UiKeyCode::Esc,
                    ctrl: false,
                    alt: false,
                }),
                _ => false,
            },
            ModalKind::InteractiveExitPrompt => match cmd {
                ModalCommand::Confirm => self.handle_interactive_exit_prompt_modal_key(UiKey {
                    code: UiKeyCode::Enter,
                    ctrl: false,
                    alt: false,
                }),
                ModalCommand::Cancel => self.handle_interactive_exit_prompt_modal_key(UiKey {
                    code: UiKeyCode::Esc,
                    ctrl: false,
                    alt: false,
                }),
                ModalCommand::Action(ModalAction::InteractiveExitInsertChar(c)) => self
                    .handle_interactive_exit_prompt_modal_key(UiKey {
                        code: UiKeyCode::Char(c),
                        ctrl: false,
                        alt: false,
                    }),
                _ => false,
            },
            ModalKind::SkipToImpl => match cmd {
                ModalCommand::Cancel => true,
                ModalCommand::Confirm | ModalCommand::Action(ModalAction::AcceptSkipToImpl) => self
                    .handle_skip_to_impl_modal_key(UiKey {
                        code: UiKeyCode::Char('y'),
                        ctrl: false,
                        alt: false,
                    }),
                ModalCommand::Action(ModalAction::DeclineSkipToImpl) => self
                    .handle_skip_to_impl_modal_key(UiKey {
                        code: UiKeyCode::Char('n'),
                        ctrl: false,
                        alt: false,
                    }),
                _ => false,
            },
            ModalKind::GitGuard => match cmd {
                ModalCommand::Cancel => true,
                ModalCommand::Confirm | ModalCommand::Action(ModalAction::GuardReset) => self
                    .handle_guard_modal_key(UiKey {
                        code: UiKeyCode::Char('r'),
                        ctrl: false,
                        alt: false,
                    }),
                ModalCommand::Action(ModalAction::GuardKeep) => {
                    self.handle_guard_modal_key(UiKey {
                        code: UiKeyCode::Char('k'),
                        ctrl: false,
                        alt: false,
                    })
                }
                _ => false,
            },
            ModalKind::StageError(stage_id) => match cmd {
                ModalCommand::Cancel => true,
                ModalCommand::Confirm | ModalCommand::Action(ModalAction::RetryStage(_)) => self
                    .handle_stage_error_modal_key(
                        stage_id,
                        UiKey {
                            code: UiKeyCode::Char('r'),
                            ctrl: false,
                            alt: false,
                        },
                    ),
                ModalCommand::Action(ModalAction::EditIdea) => self.handle_stage_error_modal_key(
                    stage_id,
                    UiKey {
                        code: UiKeyCode::Char('e'),
                        ctrl: false,
                        alt: false,
                    },
                ),
                ModalCommand::Action(ModalAction::SkipDreaming) => self
                    .handle_stage_error_modal_key(
                        stage_id,
                        UiKey {
                            code: UiKeyCode::Char('s'),
                            ctrl: false,
                            alt: false,
                        },
                    ),
                _ => false,
            },
            ModalKind::FinalValidationBlocked => match cmd {
                ModalCommand::Cancel => false,
                ModalCommand::Confirm | ModalCommand::Action(ModalAction::ForceShip) => self
                    .handle_final_validation_blocked_modal_key(UiKey {
                        code: UiKeyCode::Char('f'),
                        ctrl: false,
                        alt: false,
                    }),
                ModalCommand::Action(ModalAction::RecoverFromBlock) => self
                    .handle_final_validation_blocked_modal_key(UiKey {
                        code: UiKeyCode::Char('r'),
                        ctrl: false,
                        alt: false,
                    }),
                _ => false,
            },
            ModalKind::DreamingDecision => match cmd {
                ModalCommand::Cancel | ModalCommand::Action(ModalAction::SkipDreaming) => self
                    .handle_dreaming_decision_modal_key(UiKey {
                        code: UiKeyCode::Esc,
                        ctrl: false,
                        alt: false,
                    }),
                ModalCommand::Confirm | ModalCommand::Action(ModalAction::RunDreaming) => self
                    .handle_dreaming_decision_modal_key(UiKey {
                        code: UiKeyCode::Char('r'),
                        ctrl: false,
                        alt: false,
                    }),
                _ => false,
            },
            ModalKind::SpecReviewPaused => self.handle_spec_review_paused_modal_command(cmd),
            ModalKind::PlanReviewPaused => self.handle_plan_review_paused_modal_command(cmd),
        }
    }

    fn handle_stage_command(&mut self, cmd: StageCommand) -> bool {
        match cmd {
            StageCommand::Retry(_stage_id) => {
                if self.has_running_agent() {
                    self.retry_running_agent();
                } else {
                    self.retry_selected_target();
                }
                false
            }
            StageCommand::Approve => false,
            StageCommand::Reject => false,
            StageCommand::Start => {
                self.start_agent_manually();
                false
            }
            StageCommand::GoBack => {
                self.confirm_back = false;
                self.status_line.borrow_mut().clear();
                if self.can_go_back() {
                    self.go_back();
                }
                false
            }
        }
    }

    fn handle_modes_command(&mut self, cmd: ModesCommand) -> bool {
        match cmd {
            ModesCommand::ToggleCheap => self.toggle_cheap_mode("typed"),
            ModesCommand::SetCheap(v) => self.set_cheap_mode(v, "typed"),
            ModesCommand::ToggleYolo => self.toggle_yolo_mode("typed"),
            ModesCommand::SetYolo(v) => self.set_yolo_mode(v, "typed"),
            ModesCommand::ToggleNoninteractiveTexts => self.toggle_noninteractive_texts(),
            ModesCommand::ToggleThinkingTexts => self.toggle_thinking_texts(),
            ModesCommand::SkipToImpl => {
                if let Err(err) = self.accept_skip_to_implementation() {
                    self.record_agent_error(format!(
                        "accept skip-to-implementation failed: {err:#}"
                    ));
                }
            }
        }
        false
    }

    fn handle_split_command(&mut self, cmd: SplitCommand) -> bool {
        match cmd {
            SplitCommand::Open(target) => {
                self.open_split_target(target);
                false
            }
            SplitCommand::OpenFocused => {
                if let Some(target) = self.resolve_split_target_for_selected_row() {
                    self.open_split_target(target);
                }
                false
            }
            SplitCommand::Close => {
                self.close_split();
                false
            }
            SplitCommand::ScrollLines { delta } => {
                self.scroll_split_by_lines(delta);
                false
            }
            SplitCommand::ScrollPages { delta } => {
                self.scroll_split_by_page(delta);
                false
            }
        }
    }

    fn handle_config_panel_command(&mut self, cmd: ConfigPanelCommand) -> bool {
        // Open is the one variant routed before the panel exists; the rest
        // proxy into the existing handler.
        if let ConfigPanelCommand::Open { section } = &cmd {
            self.open_config_panel_with_arg(section.as_deref().unwrap_or(""));
            return false;
        }
        let Some(panel) = self.config_panel.as_mut() else {
            return false;
        };
        let outcome = panel.handle_command(cmd);
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
}
