use anyhow::Result;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;

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

/// Hard-wrap the input text into lines of at most `width` chars, preferring
/// word boundaries when the line has any spaces. Preserves explicit newlines.
pub fn wrap_input(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        for word in raw_line.split_inclusive(' ') {
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
                let mut remaining = word;
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
    fn wrap_input_zero_width_returns_empty() {
        assert!(wrap_input("anything", 0).is_empty());
    }

    #[test]
    fn wrap_input_preserves_explicit_newlines() {
        let lines = wrap_input("first\nsecond", 100);
        assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn wrap_input_keeps_trailing_blank_lines() {
        let lines = wrap_input("a\n\n\nb", 5);
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
    fn wrap_input_short_input_fits_one_line() {
        assert_eq!(
            wrap_input("hello world", 80),
            vec!["hello world".to_string()]
        );
    }

    #[test]
    fn wrap_input_breaks_on_word_boundary_when_possible() {
        let lines = wrap_input("alpha beta gamma", 10);
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
    fn wrap_input_hard_breaks_overlong_word() {
        // A single word longer than width must be split mid-word into chunks
        // each at most `width` chars wide.
        let lines = wrap_input("aaaaaaaaaaaaaaaaa", 5);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "aaaaa");
        assert_eq!(lines[1], "aaaaa");
        assert_eq!(lines[2], "aaaaa");
        assert_eq!(lines[3], "aa");
    }

    #[test]
    fn wrap_input_handles_unicode_by_char_count() {
        // Non-ASCII chars count as 1 char each (not bytes).
        let lines = wrap_input("héllo wörld", 5);
        assert!(
            lines.iter().all(|l| l.chars().count() <= 5),
            "lines must not exceed width by char count: {:?}",
            lines
        );
    }
}
