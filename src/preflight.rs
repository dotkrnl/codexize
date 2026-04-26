use crate::state::codexize_root;
use crate::tmux::TmuxContext;
use crate::tui::AppTerminal;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use std::{
    fs,
    path::Path,
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    NoGitEmpty,
    NoGitHasFiles,
    GitExistsNotIgnored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitState {
    None,
    GeneratingGitignore,
}

fn detect_git() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn detect_ignored(root: &Path) -> bool {
    Command::new("git")
        .args(["check-ignore", "-q"])
        .arg(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_existing_files() -> bool {
    let Ok(entries) = fs::read_dir(".") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with('.') {
            return true;
        }
    }
    false
}

fn append_to_gitignore(entry: &str) -> Result<()> {
    let path = Path::new(".gitignore");
    let mut contents = if path.exists() {
        fs::read_to_string(path).context("failed to read .gitignore")?
    } else {
        String::new()
    };

    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(entry);
    contents.push('\n');

    fs::write(path, contents).context("failed to write .gitignore")?;
    Ok(())
}

fn run_git_init() -> Result<()> {
    let status = Command::new("git")
        .arg("init")
        .status()
        .context("failed to run git init")?;
    if !status.success() {
        anyhow::bail!("git init failed with status {}", status);
    }
    Ok(())
}

fn detect_available_agent() -> Option<&'static str> {
    ["claude", "codex", "gemini", "kimi"].iter().find(|&cmd| Command::new(cmd)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)).map(|v| v as _)
}

fn detect_project_type() -> Vec<&'static str> {
    let mut types = Vec::new();

    if Path::new("Cargo.toml").exists() {
        types.push("rust");
    }
    if Path::new("package.json").exists() {
        types.push("node");
    }
    if Path::new("requirements.txt").exists()
        || Path::new("pyproject.toml").exists()
        || Path::new("setup.py").exists()
    {
        types.push("python");
    }
    if Path::new("go.mod").exists() {
        types.push("go");
    }
    if Path::new("pom.xml").exists() || Path::new("build.gradle").exists() {
        types.push("java");
    }
    if Path::new("Gemfile").exists() {
        types.push("ruby");
    }

    types
}

fn generate_heuristic_gitignore(codexize_entry: &str) -> String {
    let project_types = detect_project_type();
    let mut lines = vec![
        "# OS files",
        ".DS_Store",
        "Thumbs.db",
        "",
        "# Editor/IDE files",
        ".idea/",
        ".vscode/",
        "*.swp",
        "*.swo",
        "",
    ];

    for pt in &project_types {
        match *pt {
            "rust" => {
                lines.push("# Rust");
                lines.push("target/");
                lines.push("Cargo.lock");
                lines.push("");
            }
            "node" => {
                lines.push("# Node");
                lines.push("node_modules/");
                lines.push("dist/");
                lines.push(".npm/");
                lines.push("");
            }
            "python" => {
                lines.push("# Python");
                lines.push("__pycache__/");
                lines.push("*.pyc");
                lines.push(".venv/");
                lines.push("venv/");
                lines.push(".env");
                lines.push("");
            }
            "go" => {
                lines.push("# Go");
                lines.push("bin/");
                lines.push("");
            }
            "java" => {
                lines.push("# Java");
                lines.push("target/");
                lines.push("build/");
                lines.push("*.class");
                lines.push("");
            }
            "ruby" => {
                lines.push("# Ruby");
                lines.push(".bundle/");
                lines.push("vendor/bundle/");
                lines.push("");
            }
            _ => {}
        }
    }

    lines.push("# Codexize");
    lines.push(codexize_entry);
    lines.push("");

    lines.join("\n")
}

fn spawn_gitignore_agent(tmux: &TmuxContext, codexize_entry: &str) -> Result<std::path::PathBuf> {
    let agent = detect_available_agent();
    let finish_marker = std::env::temp_dir().join(format!(
        "codexize-gitignore-{}.done",
        std::process::id()
    ));

    if agent.is_none() {
        let content = generate_heuristic_gitignore(codexize_entry);
        fs::write(".gitignore", content).context("failed to write .gitignore")?;
        fs::write(&finish_marker, "").context("failed to write finish marker")?;
        return Ok(finish_marker);
    }

    let agent = agent.unwrap();
    let prompt = format!(
        r#"Analyze the current directory and generate a comprehensive .gitignore file.

Instructions:
1. Look at the files and directories present to identify the project type(s)
2. Include patterns for common build artifacts, IDE files, and OS files
3. MUST include this entry for the codexize orchestrator: {codexize_entry}
4. Write the .gitignore file to the current directory

After writing .gitignore, create an empty file at: {}

Do not ask for confirmation - just analyze and write the files."#,
        finish_marker.display()
    );

    let prompt_path = std::env::temp_dir().join(format!(
        "codexize-gitignore-prompt-{}.md",
        std::process::id()
    ));
    fs::write(&prompt_path, &prompt).context("failed to write agent prompt")?;

    let shell_cmd = format!(
        r#"{agent} --dangerously-skip-permissions "$(cat {prompt_path})" && touch {finish_marker}"#,
        agent = agent,
        prompt_path = prompt_path.display(),
        finish_marker = finish_marker.display(),
    );

    let window_name = "[Gitignore]";
    let status = Command::new("tmux")
        .args(["new-window", "-d", "-n", window_name, &shell_cmd])
        .status()
        .context("failed to create tmux window for gitignore agent")?;

    if !status.success() {
        anyhow::bail!("tmux new-window for gitignore agent failed");
    }

    let _ = tmux;

    Ok(finish_marker)
}

fn render_preflight_modal(frame: &mut Frame<'_>, scenario: Scenario, wait_state: WaitState) {
    let area = frame.area();
    let modal_width = area.width.saturating_sub(8).clamp(30, 72);

    let (title, body_lines): (&str, Vec<Line>) = match scenario {
        Scenario::NoGitEmpty => {
            let title = " No git repository ";
            let lines = if wait_state == WaitState::GeneratingGitignore {
                vec![
                    Line::from(""),
                    Line::from("Generating .gitignore..."),
                    Line::from(""),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from("codexize requires a git repository to"),
                    Line::from("function. Initialize one here?"),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("[Y]", Style::default().fg(Color::Green)),
                        Span::raw("/"),
                        Span::styled("Enter", Style::default().fg(Color::Green)),
                        Span::raw("  initialize git repository"),
                    ]),
                    Line::from(vec![
                        Span::styled("[Q]", Style::default().fg(Color::Red)),
                        Span::raw("        exit codexize"),
                    ]),
                ]
            };
            (title, lines)
        }
        Scenario::NoGitHasFiles => {
            let title = " No git repository ";
            let lines = if wait_state == WaitState::GeneratingGitignore {
                vec![
                    Line::from(""),
                    Line::from("Generating .gitignore..."),
                    Line::from(""),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from("codexize requires a git repository to"),
                    Line::from("function. Existing files detected — an agent"),
                    Line::from("will generate .gitignore before initializing."),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("[Y]", Style::default().fg(Color::Green)),
                        Span::raw("/"),
                        Span::styled("Enter", Style::default().fg(Color::Green)),
                        Span::raw("  generate .gitignore & init git"),
                    ]),
                    Line::from(vec![
                        Span::styled("[Q]", Style::default().fg(Color::Red)),
                        Span::raw("        exit codexize"),
                    ]),
                ]
            };
            (title, lines)
        }
        Scenario::GitExistsNotIgnored => {
            let root = codexize_root();
            let root_display = root.display().to_string();
            let title = " .codexize not in .gitignore ";
            let lines = vec![
                Line::from(""),
                Line::from(format!(
                    "Session data in {}/ is not ignored by",
                    root_display
                )),
                Line::from("git. It will appear in git status and could"),
                Line::from("be committed accidentally."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[Y]", Style::default().fg(Color::Green)),
                    Span::raw("/"),
                    Span::styled("Enter", Style::default().fg(Color::Green)),
                    Span::raw("  add to .gitignore"),
                ]),
                Line::from(vec![
                    Span::styled("[N]", Style::default().fg(Color::DarkGray)),
                    Span::raw("        continue without adding"),
                ]),
                Line::from(vec![
                    Span::styled("[Q]", Style::default().fg(Color::Red)),
                    Span::raw("        exit codexize"),
                ]),
            ];
            (title, lines)
        }
    };

    let inner_width = modal_width.saturating_sub(2) as usize;
    let wrapped: u16 = body_lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width).max(1) as u16
            }
        })
        .sum();
    let desired_height = wrapped.saturating_add(2);
    let modal_height = desired_height.min(area.height.saturating_sub(2)).max(6);

    let x = area.x + area.width.saturating_sub(modal_width) / 2;
    let y = area.y + area.height.saturating_sub(modal_height) / 2;
    let rect = Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    frame.render_widget(Clear, rect);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(body_lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, rect);
}

pub fn check(
    terminal: &mut AppTerminal,
    tmux: &TmuxContext,
) -> Result<()> {
    let has_git = detect_git();
    let root = codexize_root();
    let codexize_entry = match std::env::current_dir() {
        Ok(cwd) if root.is_absolute() => match root.strip_prefix(&cwd) {
            Ok(rel) => format!("{}/", rel.display()),
            Err(_) => format!("{}/", root.display()),
        },
        _ => format!("{}/", root.display()),
    };

    if has_git {
        if detect_ignored(&root) {
            return Ok(());
        }
        return run_gitignore_modal(terminal, Scenario::GitExistsNotIgnored, tmux, &codexize_entry);
    }

    let scenario = if has_existing_files() {
        Scenario::NoGitHasFiles
    } else {
        Scenario::NoGitEmpty
    };

    run_git_init_modal(terminal, scenario, tmux, &codexize_entry)
}

fn run_git_init_modal(
    terminal: &mut AppTerminal,
    scenario: Scenario,
    tmux: &TmuxContext,
    codexize_entry: &str,
) -> Result<()> {
    let mut wait_state = WaitState::None;
    let mut finish_marker: Option<std::path::PathBuf> = None;

    loop {
        terminal.draw(|frame| {
            render_preflight_modal(frame, scenario, wait_state);
        })?;

        if wait_state == WaitState::GeneratingGitignore {
            if let Some(ref marker) = finish_marker
                && marker.exists() {
                    run_git_init()?;
                    return Ok(());
                }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        if scenario == Scenario::NoGitHasFiles {
                            wait_state = WaitState::GeneratingGitignore;
                            finish_marker = Some(spawn_gitignore_agent(tmux, codexize_entry)?);
                        } else {
                            run_git_init()?;
                            append_to_gitignore(codexize_entry)?;
                            return Ok(());
                        }
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
    }
}

fn run_gitignore_modal(
    terminal: &mut AppTerminal,
    scenario: Scenario,
    _tmux: &TmuxContext,
    codexize_entry: &str,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            render_preflight_modal(frame, scenario, WaitState::None);
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        append_to_gitignore(codexize_entry)?;
                        return Ok(());
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        return Ok(());
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fs_lock;

    fn with_temp_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let prev = std::env::current_dir().unwrap();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::env::set_current_dir(dir.path()).unwrap();
            f()
        }));

        std::env::set_current_dir(prev).unwrap();
        result.unwrap()
    }

    #[test]
    fn test_detect_project_type_rust() {
        with_temp_dir(|| {
            fs::write("Cargo.toml", "[package]\nname = \"test\"").unwrap();
            let types = detect_project_type();
            assert!(types.contains(&"rust"));
        });
    }

    #[test]
    fn test_has_existing_files_empty() {
        with_temp_dir(|| {
            assert!(!has_existing_files());
        });
    }

    #[test]
    fn test_has_existing_files_with_dotfile() {
        with_temp_dir(|| {
            fs::write(".hidden", "").unwrap();
            assert!(!has_existing_files());
        });
    }

    #[test]
    fn test_has_existing_files_with_regular_file() {
        with_temp_dir(|| {
            fs::write("file.txt", "content").unwrap();
            assert!(has_existing_files());
        });
    }

    #[test]
    fn test_generate_heuristic_gitignore_contains_codexize() {
        let content = generate_heuristic_gitignore(".codexize/");
        assert!(content.contains(".codexize/"));
        assert!(content.contains(".DS_Store"));
    }

    #[test]
    fn test_append_to_gitignore_creates_file() {
        with_temp_dir(|| {
            append_to_gitignore(".codexize/").unwrap();
            let content = fs::read_to_string(".gitignore").unwrap();
            assert!(content.contains(".codexize/"));
        });
    }

    #[test]
    fn test_append_to_gitignore_appends() {
        with_temp_dir(|| {
            fs::write(".gitignore", "node_modules/").unwrap();
            append_to_gitignore(".codexize/").unwrap();
            let content = fs::read_to_string(".gitignore").unwrap();
            assert!(content.contains("node_modules/"));
            assert!(content.contains(".codexize/"));
        });
    }
}
