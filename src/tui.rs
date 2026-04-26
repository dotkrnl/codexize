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
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    Ok(terminal)
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
