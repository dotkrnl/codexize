use crate::app_runtime::{AppCommand, AppView, ModalKind, UiKey, UiKeyCode};
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
pub fn render_app<F>(terminal: &mut AppTerminal, _view: &AppView, draw: F) -> Result<()>
where
    F: FnOnce(&mut ratatui::Frame<'_>),
{
    terminal.draw(draw)?;
    Ok(())
}
pub fn poll_command(timeout: Duration, view: &AppView) -> Result<Option<AppCommand>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    Ok(command_from_event(event::read()?, view))
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
            Ok(Some(event)) => Ok(command_from_event(event, view)),
            Ok(None) | Err(_) => Ok(None),
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
            let _ = crate::data::async_bridge::block_on_io(handle);
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
        Event::Paste(text) => Some(AppCommand::PasteInput { text }),
        _ => None,
    }
}
fn command_from_key_event(key: KeyEvent, view: &AppView) -> Option<AppCommand> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    // Esc on the quit-confirmation modal is unambiguously "cancel the
    // confirmation": the modal handler clears `pending_quit_confirmation_run_id`
    // and stays on screen. Translating it at the seam exercises an
    // operator-intent variant in production rather than routing through the
    // generic `KeyPress` bridge.
    if matches!(view.modal, Some(ModalKind::QuitRunningAgent)) && key.modifiers.is_empty() {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Some(AppCommand::ConfirmModal)
            }
            KeyCode::Esc
            | KeyCode::Char('n')
            | KeyCode::Char('N')
            | KeyCode::Char('q')
            | KeyCode::Char('Q') => Some(AppCommand::CancelModal),
            _ => None,
        };
    }
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
    Some(AppCommand::KeyPress(UiKey {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
    }))
}
/// Strip ANSI escape sequences (CSI form `ESC[…<final-byte>`) from `s`.
/// Shared across the TUI so chat transcript wrapping, validation report
/// rendering, and live-summary sanitation all agree on what counts as a
/// printable column.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
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
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        let clean = strip_ansi(raw_line);
        if clean.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        for word in clean.split_inclusive(' ') {
            let word_len = word.chars().count();
            if current_len + word_len <= width {
                current.push_str(word);
                current_len += word_len;
                continue;
            }
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                current_len = 0;
            }
            if word_len <= width {
                current.push_str(word);
                current_len = word_len;
            } else {
                let mut remaining: &str = word;
                while remaining.chars().count() > width {
                    let split_at = remaining
                        .char_indices()
                        .nth(width)
                        .map_or(remaining.len(), |(i, _)| i);
                    out.push(remaining[..split_at].to_string());
                    remaining = &remaining[split_at..];
                }
                if !remaining.is_empty() {
                    current.push_str(remaining);
                    current_len = remaining.chars().count();
                }
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}
#[cfg(test)]
#[path = "tui_tests.rs"]
mod tests;
