use crate::app::keys::{UiKey, UiKeyCode};
use crate::app_runtime::commands::{
    GlobalCommand, InputCommand, ModalAction, ModalCommand, SessionCommand, ShellCommand,
};
use crate::app_runtime::{AppCommand, AppView, ModalKind};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    style::Style,
    text::{Line, Span},
};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
pub type AppTerminal = Terminal<CrosstermBackend<io::Stdout>>;
pub fn start() -> Result<AppTerminal> {
    enable_raw_mode()?;
    let result = (|| -> Result<AppTerminal> {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(terminal)
    })();
    match result {
        Ok(terminal) => Ok(terminal),
        Err(err) => {
            restore_terminal_after_failed_start();
            Err(err)
        }
    }
}
/// Best-effort restoration after `start()` partially succeeds and then
/// fails (e.g. `Terminal::new` returns `Err` once raw mode, the alternate
/// screen, and bracketed paste are already armed). Any individual step
/// that fails is swallowed because we are already on the error path.
fn restore_terminal_after_failed_start() {
    let mut stdout = io::stdout();
    let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
pub fn stop(terminal: &mut AppTerminal) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}
/// Temporarily drops out of the TUI so an external program (e.g. vim) can
/// own the terminal, then restores the alternate screen on return.
pub fn run_foreground<F>(terminal: &mut AppTerminal, f: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    let outcome = f();
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableBracketedPaste
    )?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    outcome
}
pub fn render_app<F>(terminal: &mut AppTerminal, draw: F) -> Result<()>
where
    F: FnOnce(&mut ratatui::Frame<'_>),
{
    terminal.draw(draw)?;
    Ok(())
}
pub struct CrosstermInputAdapter {
    rx: mpsc::UnboundedReceiver<Event>,
    cancel: CancellationToken,
    worker: Option<tokio::task::JoinHandle<()>>,
}
impl CrosstermInputAdapter {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        // Crossterm's event API is blocking and terminal-global; keep it on
        // tokio's blocking pool while the synchronous render loop only drains
        // already-adapted events from an async channel.
        let worker = tokio::task::spawn_blocking(move || {
            while !worker_cancel.is_cancelled() {
                match event::poll(Duration::from_millis(50)) {
                    Ok(true) => {
                        let Ok(event) = event::read() else {
                            break;
                        };
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            rx,
            cancel,
            worker: Some(worker),
        }
    }
    pub fn next_command(
        &mut self,
        timeout: Duration,
        view: &AppView,
    ) -> Result<Option<AppCommand>> {
        match crate::data::async_bridge::block_on_io(tokio::time::timeout(timeout, self.rx.recv()))
        {
            Ok(Ok(Some(event))) => Ok(command_from_event(event, view)),
            Ok(Ok(None) | Err(_)) => Ok(None),
            Err(err) => {
                tracing::warn!("event poll bridge failed: {err}");
                Ok(None)
            }
        }
    }
    /// Cancel the blocking-poll worker and wait for it to exit. Used
    /// before handing the terminal to a foreground program (`$EDITOR`,
    /// vim) so the worker's `event::poll` / `event::read` loop cannot
    /// race the foreground program for keystrokes coming off the same
    /// TTY. Buffered events arriving after the cancel are dropped via
    /// `Self::drop` since they were intended for the now-superseded
    /// alt-screen UI, not for the foreground program.
    pub fn shutdown_blocking(mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.worker.take() {
            // The worker checks the cancellation flag once per `event::poll`
            // window (≤ 50 ms), so this join completes promptly.
            if let Err(err) = crate::data::async_bridge::block_on_io(handle) {
                tracing::warn!("input worker join failed: {err}");
            }
        }
    }
}
impl Drop for CrosstermInputAdapter {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub fn command_from_event(event: Event, view: &AppView) -> Option<AppCommand> {
    match event {
        Event::Key(key) => command_from_key_event(key, view),
        Event::Paste(text) => Some(AppCommand::Session(
            view.session_id.clone(),
            SessionCommand::Input(InputCommand::InsertText(text)),
        )),
        _ => None,
    }
}
fn command_from_key_event(key: KeyEvent, view: &AppView) -> Option<AppCommand> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let session_id = view.session_id.clone();
    // Esc on the quit-confirmation modal is unambiguously "cancel the
    // confirmation": the modal handler clears `pending_quit_confirmation_run_id`
    // and stays on screen. Translating it at the seam exercises an
    // operator-intent variant in production rather than routing through the
    // generic `KeyPress` bridge.
    if matches!(view.modal, Some(ModalKind::QuitRunningAgent)) && key.modifiers.is_empty() {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') => Some(AppCommand::Session(
                session_id,
                SessionCommand::Modal(ModalCommand::Confirm),
            )),
            KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q' | 'Q') => Some(AppCommand::Session(
                session_id,
                SessionCommand::Modal(ModalCommand::Cancel),
            )),
            _ => None,
        };
    }
    translate_key_to_command(ui_key_from_event(key), view)
}

fn translate_key_to_command(key: UiKey, view: &AppView) -> Option<AppCommand> {
    let session_id = view.session_id.clone();

    // Truly global keys
    if key.code == UiKeyCode::Char('c') && key.ctrl {
        return Some(AppCommand::Global(GlobalCommand::StopRunningAgent));
    }

    // Modal-specific translation
    if let Some(modal) = view.modal {
        match modal {
            ModalKind::QuitRunningAgent
            | ModalKind::CancelSession
            | ModalKind::GitGuard
            | ModalKind::SkipToImpl
            | ModalKind::StageError(_)
            | ModalKind::FinalValidationBlocked
            | ModalKind::DreamingDecision
            | ModalKind::SpecReviewPaused
            | ModalKind::PlanReviewPaused => match key.code {
                UiKeyCode::Enter => {
                    return Some(AppCommand::Session(
                        session_id.clone(),
                        SessionCommand::Modal(ModalCommand::Confirm),
                    ));
                }
                UiKeyCode::Esc => {
                    return Some(AppCommand::Session(
                        session_id.clone(),
                        SessionCommand::Modal(ModalCommand::Cancel),
                    ));
                }
                _ => {}
            },
            ModalKind::InteractiveExitPrompt => match key.code {
                UiKeyCode::Enter => {
                    return Some(AppCommand::Session(
                        session_id.clone(),
                        SessionCommand::Modal(ModalCommand::Confirm),
                    ));
                }
                UiKeyCode::Esc => {
                    return Some(AppCommand::Session(
                        session_id.clone(),
                        SessionCommand::Modal(ModalCommand::Cancel),
                    ));
                }
                UiKeyCode::Char(c) if !key.ctrl && !key.alt => {
                    return Some(AppCommand::Session(
                        session_id.clone(),
                        SessionCommand::Modal(ModalCommand::Action(
                            ModalAction::InteractiveExitInsertChar(c),
                        )),
                    ));
                }
                _ => {}
            },
        }
    }

    // Palette toggle
    if key.code == UiKeyCode::Char(':') && !key.ctrl && !key.alt {
        return Some(AppCommand::Session(
            session_id,
            SessionCommand::Palette(crate::app_runtime::commands::PaletteCommand::Open),
        ));
    }

    // Palette ghost completion (Tab)
    if key.code == UiKeyCode::Tab && !key.ctrl && !key.alt {
        return Some(AppCommand::Session(
            session_id,
            SessionCommand::Palette(crate::app_runtime::commands::PaletteCommand::AcceptGhost),
        ));
    }

    // Shell-level navigation (sidebar)
    match key.code {
        UiKeyCode::Left | UiKeyCode::Right if !key.ctrl && !key.alt => {
            return Some(AppCommand::Shell(ShellCommand::ToggleSidebarFocus));
        }
        UiKeyCode::Up if !key.ctrl && !key.alt => {
            return Some(AppCommand::Shell(ShellCommand::MoveSidebarSelection {
                delta: -1,
            }));
        }
        UiKeyCode::Down if !key.ctrl && !key.alt => {
            return Some(AppCommand::Shell(ShellCommand::MoveSidebarSelection {
                delta: 1,
            }));
        }
        UiKeyCode::Enter if !key.ctrl && !key.alt => {
            return Some(AppCommand::Shell(ShellCommand::OpenSelectedSidebarSession));
        }
        UiKeyCode::Esc if !key.ctrl && !key.alt => {
            return Some(AppCommand::Shell(ShellCommand::CloseSidebar));
        }
        _ => {}
    }

    // App-level navigation (tree)
    match key.code {
        UiKeyCode::Up => {
            return Some(AppCommand::Session(
                session_id,
                SessionCommand::Tree(
                    crate::app_runtime::commands::TreeCommand::ScrollOrMoveFocus { delta: -1 },
                ),
            ));
        }
        UiKeyCode::Down => {
            return Some(AppCommand::Session(
                session_id,
                SessionCommand::Tree(
                    crate::app_runtime::commands::TreeCommand::ScrollOrMoveFocus { delta: 1 },
                ),
            ));
        }
        UiKeyCode::PageUp => {
            return Some(AppCommand::Session(
                session_id,
                SessionCommand::Tree(
                    crate::app_runtime::commands::TreeCommand::ScrollViewportPage { delta: -1 },
                ),
            ));
        }
        UiKeyCode::PageDown => {
            return Some(AppCommand::Session(
                session_id,
                SessionCommand::Tree(
                    crate::app_runtime::commands::TreeCommand::ScrollViewportPage { delta: 1 },
                ),
            ));
        }
        UiKeyCode::Char(' ') if !key.ctrl && !key.alt => {
            return Some(AppCommand::Session(
                session_id,
                SessionCommand::Tree(crate::app_runtime::commands::TreeCommand::ToggleExpand),
            ));
        }
        UiKeyCode::Enter if !key.ctrl && !key.alt => {
            return Some(AppCommand::Session(
                session_id,
                SessionCommand::Tree(crate::app_runtime::commands::TreeCommand::ActivateFocused),
            ));
        }
        _ => {}
    }

    // Input editing fallback
    match key.code {
        UiKeyCode::Char(c) if !key.ctrl && !key.alt => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::InsertText(c.to_string())),
        )),
        UiKeyCode::Backspace => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::Backspace),
        )),
        UiKeyCode::Delete => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::DeleteForward),
        )),
        UiKeyCode::Left => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::MoveCursor(
                crate::app_runtime::commands::CursorMove::Left,
            )),
        )),
        UiKeyCode::Right => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::MoveCursor(
                crate::app_runtime::commands::CursorMove::Right,
            )),
        )),
        UiKeyCode::Home => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::MoveCursor(
                crate::app_runtime::commands::CursorMove::Home,
            )),
        )),
        UiKeyCode::End => Some(AppCommand::Session(
            session_id,
            SessionCommand::Input(InputCommand::MoveCursor(
                crate::app_runtime::commands::CursorMove::End,
            )),
        )),
        _ => None,
    }
}

pub(crate) fn ui_key_from_event(key: KeyEvent) -> UiKey {
    let code = match key.code {
        KeyCode::Esc => UiKeyCode::Esc,
        KeyCode::Enter => UiKeyCode::Enter,
        KeyCode::Backspace => UiKeyCode::Backspace,
        KeyCode::Delete => UiKeyCode::Delete,
        KeyCode::Left => UiKeyCode::Left,
        KeyCode::Right => UiKeyCode::Right,
        KeyCode::Home => UiKeyCode::Home,
        KeyCode::End => UiKeyCode::End,
        KeyCode::Up => UiKeyCode::Up,
        KeyCode::Down => UiKeyCode::Down,
        KeyCode::PageUp => UiKeyCode::PageUp,
        KeyCode::PageDown => UiKeyCode::PageDown,
        KeyCode::Tab => UiKeyCode::Tab,
        KeyCode::BackTab => UiKeyCode::BackTab,
        KeyCode::Char(c) => UiKeyCode::Char(c),
        _ => UiKeyCode::Unknown,
    };
    UiKey {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
    }
}

impl From<KeyEvent> for UiKey {
    fn from(key: KeyEvent) -> Self {
        ui_key_from_event(key)
    }
}
/// Strip ANSI escape sequences (CSI form `ESC[…<final-byte>`) from `s`.
/// Shared across the TUI so chat transcript wrapping, validation report
/// rendering, and live-summary sanitation all agree on what counts as a
/// printable column.
pub fn strip_ansi(s: &str) -> String {
    crate::app_runtime::views::render::strip_ansi(s)
}
/// Build a sequence of [`Line`]s for "<prefix><body>" where `body` is wrapped
/// to fit and continuation lines indent to align under the body's first
/// column. The single point that every transcript-shaped renderer (chat
/// messages, final-validation reports, status surfaces) routes through, so a
/// missing wrap call now shows up as a search hit on this function rather
/// than as a silent overflow on a forgotten code path.
///
/// `prefix_visible_width` is the printable column count of `first_line_prefix`
/// — the caller knows it (it computes the prefix), and we use it both to
/// derive the wrap budget and to align continuation lines.
///
/// If `body` produces no wrapped chunks (empty input or `body_width == 0`),
/// the function still emits a single line carrying just the prefix so a
/// labelled-but-empty field stays visible. Callers that want to skip the
/// prefix entry on empty body should check before calling.
pub fn wrap_lines_with_prefix(
    first_line_prefix: Vec<Span<'static>>,
    prefix_visible_width: usize,
    body: &str,
    body_style: Style,
    available_width: usize,
) -> Vec<Line<'static>> {
    let body_width = available_width.saturating_sub(prefix_visible_width).max(1);
    let wrapped = wrap_text(body, body_width);
    if wrapped.is_empty() {
        return vec![Line::from(first_line_prefix)];
    }
    let cont_indent: String = " ".repeat(prefix_visible_width);
    let mut lines = Vec::with_capacity(wrapped.len());
    let mut iter = wrapped.into_iter();
    let first = iter.next().expect("non-empty wrapped");
    let mut first_spans = first_line_prefix;
    first_spans.push(Span::styled(first, body_style));
    lines.push(Line::from(first_spans));
    for chunk in iter {
        lines.push(Line::from(vec![
            Span::raw(cont_indent.clone()),
            Span::styled(chunk, body_style),
        ]));
    }
    lines
}
/// Hard-wrap `text` into lines of at most `width` printable chars, preferring
/// word boundaries when the line has any spaces. Preserves explicit newlines
/// and strips ANSI escape sequences first so width math counts only what the
/// terminal actually renders.
///
/// This is the single text-wrap helper for the TUI — chat messages, input
/// sheets, palette/picker hints, validation reports, and modal bodies all
/// route through it so width handling stays consistent (and so a missing
/// wrap call shows up as a search hit).
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    crate::app_runtime::views::render::wrap_text(text, width)
}
#[cfg(test)]
#[path = "tui_tests.rs"]
mod tests;
