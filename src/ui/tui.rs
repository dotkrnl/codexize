use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::Duration;

use crate::app_runtime::{AppCommand, AppView, ModalKind, UiKey, UiKeyCode};

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
                        .map(|(i, _)| i)
                        .unwrap_or(remaining.len());
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
mod tests {
    use super::*;

    #[test]
    fn restore_terminal_after_failed_start_is_idempotent() {
        // Cleanup must complete without panicking even when raw mode is not
        // engaged and no alternate screen has been entered, so the failure
        // path of `start()` cannot make a bad situation worse.
        restore_terminal_after_failed_start();
        restore_terminal_after_failed_start();
        // After cleanup, raw mode must not be left engaged.
        assert!(
            !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false),
            "raw mode must be disabled after the cleanup helper runs"
        );
    }

    #[test]
    fn wrap_text_zero_width_returns_empty() {
        assert!(wrap_text("anything", 0).is_empty());
    }

    #[test]
    fn quit_running_agent_modal_keys_become_domain_commands() {
        let mut view = AppView::empty("ui-command-test");
        view.modal = Some(ModalKind::QuitRunningAgent);

        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let cancel = Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        assert_eq!(
            command_from_event(enter, &view),
            Some(AppCommand::ConfirmModal)
        );
        assert_eq!(
            command_from_event(cancel, &view),
            Some(AppCommand::CancelModal)
        );
    }

    #[test]
    fn wrap_text_preserves_explicit_newlines() {
        let lines = wrap_text("first\nsecond", 100);
        assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn wrap_text_keeps_trailing_blank_lines() {
        let lines = wrap_text("a\n\n\nb", 5);
        assert_eq!(
            lines,
            vec![
                "a".to_string(),
                String::new(),
                String::new(),
                "b".to_string()
            ]
        );
    }

    #[test]
    fn wrap_text_short_input_fits_one_line() {
        assert_eq!(
            wrap_text("hello world", 80),
            vec!["hello world".to_string()]
        );
    }

    #[test]
    fn wrap_text_breaks_on_word_boundary_when_possible() {
        let lines = wrap_text("alpha beta gamma", 10);
        // "alpha beta" is 10 chars (incl. trailing space "alpha beta " = 11),
        // so the wrap point should split at the first space that overflows.
        assert!(
            lines.iter().all(|l| l.chars().count() <= 10),
            "all wrapped lines must fit width=10: {:?}",
            lines
        );
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn wrap_text_hard_breaks_overlong_word() {
        // A single word longer than width must be split mid-word into chunks
        // each at most `width` chars wide.
        let lines = wrap_text("aaaaaaaaaaaaaaaaa", 5);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "aaaaa");
        assert_eq!(lines[1], "aaaaa");
        assert_eq!(lines[2], "aaaaa");
        assert_eq!(lines[3], "aa");
    }

    #[test]
    fn wrap_text_handles_unicode_by_char_count() {
        // Non-ASCII chars count as 1 char each (not bytes).
        let lines = wrap_text("héllo wörld", 5);
        assert!(
            lines.iter().all(|l| l.chars().count() <= 5),
            "lines must not exceed width by char count: {:?}",
            lines
        );
    }
}
