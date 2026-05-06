//! Terminal half of the preflight flow.
//!
//! Owns rendering of the preflight modals and the operator key loop. Backend
//! probes and side-effect installers live in [`crate::data::preflight`]; this
//! module turns those typed results into ratatui frames and dispatches the
//! chosen action back through the data layer.

use crate::data::preflight as backend;
pub use crate::data::preflight::PreflightOutcome;
use crate::data::preflight::Scenario;
use crate::state::codexize_root;
use crate::tui::{AppTerminal, wrap_text};
use crate::ui::chrome::modal::{modal_inner_width, render_modal_overlay};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    style::{Color, Style},
    text::{Line, Span},
};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalAction {
    Accept,
    Skip,
    Exit,
    Ignore,
}

fn preflight_modal_content(scenario: Scenario) -> (&'static str, Vec<String>, Line<'static>) {
    match scenario {
        Scenario::NoGitEmpty => {
            let title = " No git repository ";
            (
                title,
                vec![
                    "codexize requires a git repository to function. Initialize one here?"
                        .to_string(),
                ],
                preflight_keymap_line(&[
                    (
                        "[Y]",
                        Color::Green,
                        "Enter",
                        Some(Color::Green),
                        "initialize git repository",
                    ),
                    ("[Q]", Color::Red, "", None, "exit codexize"),
                ]),
            )
        }
        Scenario::NoGitHasFiles => {
            let title = " No git repository ";
            (
                title,
                vec![
                    "codexize requires a git repository to function. Existing files detected — it will generate .gitignore before initializing.".to_string(),
                ],
                preflight_keymap_line(&[
                    (
                        "[Y]",
                        Color::Green,
                        "Enter",
                        Some(Color::Green),
                        "generate .gitignore & init git",
                    ),
                    ("[Q]", Color::Red, "", None, "exit codexize"),
                ]),
            )
        }
        Scenario::GitExistsNotIgnored => {
            let root = codexize_root();
            let root_display = root.display().to_string();
            let title = " .codexize not in .gitignore ";
            (
                title,
                vec![format!(
                    "Session data in {root_display}/ is not ignored by git. It will appear in git status and could be committed accidentally."
                )],
                preflight_keymap_line(&[
                    (
                        "[Y]",
                        Color::Green,
                        "Enter",
                        Some(Color::Green),
                        "add to .gitignore",
                    ),
                    // Keep optional skip markers light so DarkGray stays
                    // reserved for backdrop/chrome, matching the shared modal contract.
                    ("[N]", Color::Gray, "", None, "continue without adding"),
                    ("[Q]", Color::Red, "", None, "exit codexize"),
                ]),
            )
        }
        Scenario::CodexAcpMissing => {
            let title = " Codex ACP not installed ";
            (
                title,
                vec![
                    "Codex CLI is installed, but codex-acp is missing. Install it with Homebrew?"
                        .to_string(),
                ],
                preflight_keymap_line(&[
                    (
                        "[Y]",
                        Color::Green,
                        "Enter",
                        Some(Color::Green),
                        "brew install codex-acp",
                    ),
                    ("[N]", Color::Gray, "Esc", Some(Color::Gray), "skip"),
                ]),
            )
        }
        Scenario::ClaudeAcpMissing => {
            let root = crate::acp::claude_acp_install_root();
            let title = " Claude ACP not installed ";
            (
                title,
                vec![format!(
                    "Claude CLI is installed, but claude-agent-acp is missing. Install it under {}?",
                    root.display()
                )],
                preflight_keymap_line(&[
                    (
                        "[Y]",
                        Color::Green,
                        "Enter",
                        Some(Color::Green),
                        "install Claude ACP",
                    ),
                    ("[N]", Color::Gray, "Esc", Some(Color::Gray), "skip"),
                ]),
            )
        }
    }
}

fn render_preflight_modal(frame: &mut Frame<'_>, scenario: Scenario) {
    let (title, body_copy, keymap_line) = preflight_modal_content(scenario);
    let body_lines = preflight_body_lines(frame.area(), body_copy);
    render_modal_overlay(
        frame,
        frame.area(),
        Color::Yellow,
        Some(title),
        body_lines,
        keymap_line,
    );
}

fn preflight_body_lines(
    area: ratatui::layout::Rect,
    paragraphs: Vec<String>,
) -> Vec<Line<'static>> {
    let inner_width = modal_inner_width(area) as usize;
    let mut lines = Vec::new();
    for (idx, paragraph) in paragraphs.into_iter().enumerate() {
        if idx > 0 {
            lines.push(Line::from(""));
        }
        for wrapped in wrap_text(&paragraph, inner_width.max(1)) {
            lines.push(Line::from(Span::styled(
                wrapped,
                Style::default().fg(Color::White),
            )));
        }
    }
    lines
}

fn preflight_keymap_line(actions: &[(&str, Color, &str, Option<Color>, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, (marker, marker_color, alternate, alternate_color, action)) in
        actions.iter().enumerate()
    {
        if idx > 0 {
            spans.push(Span::styled("  ·  ", Style::default().fg(Color::Gray)));
        }
        spans.push(Span::styled(
            (*marker).to_string(),
            Style::default().fg(*marker_color),
        ));
        if !alternate.is_empty() {
            spans.push(Span::raw("/"));
            spans.push(Span::styled(
                (*alternate).to_string(),
                Style::default().fg(alternate_color.unwrap_or(*marker_color)),
            ));
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            (*action).to_string(),
            Style::default().fg(Color::Gray),
        ));
    }
    Line::from(spans)
}

pub fn check(terminal: &mut AppTerminal) -> Result<PreflightOutcome> {
    let has_git = backend::detect_git();
    let root = codexize_root();
    let codexize_entry = match std::env::current_dir() {
        Ok(cwd) if root.is_absolute() => match root.strip_prefix(&cwd) {
            Ok(rel) => format!("{}/", rel.display()),
            Err(_) => format!("{}/", root.display()),
        },
        _ => format!("{}/", root.display()),
    };

    if has_git {
        if backend::detect_ignored(&root) {
            run_acp_install_modals_if_missing(terminal)?;
            return Ok(PreflightOutcome::Continue);
        }
        if run_gitignore_modal(terminal, Scenario::GitExistsNotIgnored, &codexize_entry)?
            == PreflightOutcome::Exit
        {
            return Ok(PreflightOutcome::Exit);
        }
        run_acp_install_modals_if_missing(terminal)?;
        return Ok(PreflightOutcome::Continue);
    }

    let scenario = if backend::has_existing_files() {
        Scenario::NoGitHasFiles
    } else {
        Scenario::NoGitEmpty
    };

    if run_git_init_modal(terminal, scenario, &codexize_entry)? == PreflightOutcome::Exit {
        return Ok(PreflightOutcome::Exit);
    }
    run_acp_install_modals_if_missing(terminal)?;
    Ok(PreflightOutcome::Continue)
}

fn run_git_init_modal(
    terminal: &mut AppTerminal,
    scenario: Scenario,
    codexize_entry: &str,
) -> Result<PreflightOutcome> {
    loop {
        terminal.draw(|frame| {
            render_preflight_modal(frame, scenario);
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match classify_required_modal_key(key.code) {
                ModalAction::Accept => {
                    if scenario == Scenario::NoGitHasFiles {
                        let finish_marker =
                            backend::generate_gitignore_preflight_file(codexize_entry)?;
                        debug_assert!(
                            finish_marker.exists(),
                            "deterministic preflight generation should create its marker eagerly"
                        );
                        backend::run_git_init()?;
                        return Ok(PreflightOutcome::Continue);
                    }
                    backend::run_git_init()?;
                    backend::append_to_gitignore(codexize_entry)?;
                    return Ok(PreflightOutcome::Continue);
                }
                ModalAction::Exit => return Ok(PreflightOutcome::Exit),
                ModalAction::Ignore | ModalAction::Skip => {}
            }
        }
    }
}

fn run_gitignore_modal(
    terminal: &mut AppTerminal,
    scenario: Scenario,
    codexize_entry: &str,
) -> Result<PreflightOutcome> {
    loop {
        terminal.draw(|frame| {
            render_preflight_modal(frame, scenario);
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match classify_gitignore_modal_key(key.code) {
                ModalAction::Accept => {
                    backend::append_to_gitignore(codexize_entry)?;
                    backend::maybe_auto_commit_gitignore(|_| {});
                    return Ok(PreflightOutcome::Continue);
                }
                ModalAction::Skip => return Ok(PreflightOutcome::Continue),
                ModalAction::Exit => return Ok(PreflightOutcome::Exit),
                ModalAction::Ignore => {}
            }
        }
    }
}

fn run_acp_install_modals_if_missing(terminal: &mut AppTerminal) -> Result<()> {
    if crate::acp::should_offer_codex_acp_install() {
        run_acp_install_modal(
            terminal,
            Scenario::CodexAcpMissing,
            backend::install_codex_acp,
        )?;
    }
    if crate::acp::should_offer_claude_acp_install() {
        run_acp_install_modal(
            terminal,
            Scenario::ClaudeAcpMissing,
            backend::install_claude_acp,
        )?;
    }
    Ok(())
}

fn run_acp_install_modal(
    terminal: &mut AppTerminal,
    scenario: Scenario,
    install: fn() -> Result<()>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            render_preflight_modal(frame, scenario);
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match classify_optional_modal_key(key.code) {
                ModalAction::Accept => {
                    install()?;
                    return Ok(());
                }
                ModalAction::Skip | ModalAction::Exit => return Ok(()),
                ModalAction::Ignore => {}
            }
        }
    }
}

fn classify_required_modal_key(key: KeyCode) -> ModalAction {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => ModalAction::Accept,
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => ModalAction::Exit,
        _ => ModalAction::Ignore,
    }
}

fn classify_gitignore_modal_key(key: KeyCode) -> ModalAction {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => ModalAction::Accept,
        KeyCode::Char('n') | KeyCode::Char('N') => ModalAction::Skip,
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => ModalAction::Exit,
        _ => ModalAction::Ignore,
    }
}

fn classify_optional_modal_key(key: KeyCode) -> ModalAction {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => ModalAction::Accept,
        KeyCode::Char('n')
        | KeyCode::Char('N')
        | KeyCode::Char('q')
        | KeyCode::Char('Q')
        | KeyCode::Esc => ModalAction::Skip,
        _ => ModalAction::Ignore,
    }
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
