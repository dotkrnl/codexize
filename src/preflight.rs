use crate::app::chrome::modal::{modal_inner_width, render_modal_overlay};
use crate::state::codexize_root;
use crate::tui::{AppTerminal, wrap_input};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    style::{Color, Style},
    text::{Line, Span},
};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightOutcome {
    Continue,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    NoGitEmpty,
    NoGitHasFiles,
    GitExistsNotIgnored,
    CodexAcpMissing,
    ClaudeAcpMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalAction {
    Accept,
    Skip,
    Exit,
    Ignore,
}

const GITIGNORE_AUTO_COMMIT_SUBJECT: &str = "chore: ignore .codexize session data";

fn detect_git() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn detect_ignored(root: &Path) -> bool {
    let ignored = |path: &std::ffi::OsStr| {
        Command::new("git")
            .args(["check-ignore", "-q", "--no-index", "--"])
            .arg(path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };

    if ignored(root.as_os_str()) {
        return true;
    }

    let dir_form = format!("{}/", root.display());
    ignored(std::ffi::OsStr::new(&dir_form))
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

fn is_codexize_status_path(path: &str) -> bool {
    path == ".codexize" || path.starts_with(".codexize/")
}

fn accepted_gitignore_status(short_status: &str) -> bool {
    matches!(short_status, " M" | "M " | "MM" | "??" | "A ")
}

fn parse_porcelain_line(line: &str) -> Option<(&str, &str)> {
    if line.len() < 4 {
        return None;
    }
    Some((&line[..2], &line[3..]))
}

fn run_git_command_with_stderr(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run `git {}`: {e}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let detail = if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        stderr
    };
    Err(format!("`git {}` failed: {}", args.join(" "), detail))
}

fn maybe_auto_commit_gitignore<F>(mut warn: F)
where
    F: FnMut(String),
{
    let output = match Command::new("git")
        .args(["status", "--porcelain=v1", "-uall"])
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            warn(format!(
                "warning: failed to run git status for .gitignore auto-commit: {err}"
            ));
            return;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        };
        warn(format!(
            "warning: failed to run git status for .gitignore auto-commit: {detail}"
        ));
        return;
    }

    let mut filtered: Vec<(String, String)> = Vec::new();
    let porcelain = String::from_utf8_lossy(&output.stdout);
    for line in porcelain.lines() {
        let Some((short_status, path)) = parse_porcelain_line(line) else {
            // Ambiguous porcelain output should conservatively skip auto-commit.
            return;
        };
        if is_codexize_status_path(path) {
            continue;
        }
        filtered.push((short_status.to_string(), path.to_string()));
    }

    let [(status, path)] = filtered.as_slice() else {
        return;
    };
    if path != ".gitignore" || !accepted_gitignore_status(status) {
        return;
    }

    if let Err(err) = run_git_command_with_stderr(&["add", ".gitignore"]) {
        warn(format!("warning: .gitignore auto-commit skipped: {err}"));
        return;
    }
    if let Err(err) = run_git_command_with_stderr(&["commit", "-m", GITIGNORE_AUTO_COMMIT_SUBJECT])
    {
        warn(format!("warning: .gitignore auto-commit skipped: {err}"));
    }
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

fn install_claude_acp() -> Result<()> {
    let root = crate::acp::claude_acp_install_root();
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    let status = Command::new("npm")
        .args([
            "install",
            "--prefix",
            root.to_string_lossy().as_ref(),
            "@agentclientprotocol/claude-agent-acp",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run npm install for Claude ACP")?;
    if !status.success() {
        anyhow::bail!("Claude ACP install failed with status {}", status);
    }
    Ok(())
}

fn install_codex_acp() -> Result<()> {
    let status = Command::new("brew")
        .args(["install", "codex-acp"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run brew install codex-acp")?;
    if !status.success() {
        anyhow::bail!("Codex ACP install failed with status {}", status);
    }
    Ok(())
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

fn generate_gitignore_preflight_file(codexize_entry: &str) -> Result<std::path::PathBuf> {
    let finish_marker =
        std::env::temp_dir().join(format!("codexize-gitignore-{}.done", std::process::id()));
    // Preflight intentionally stays deterministic here instead of opening an
    // ACP session before codexize has created session state for a real run.
    let content = generate_heuristic_gitignore(codexize_entry);
    fs::write(".gitignore", content).context("failed to write .gitignore")?;
    fs::write(&finish_marker, "").context("failed to write finish marker")?;
    Ok(finish_marker)
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
        for wrapped in wrap_input(&paragraph, inner_width.max(1)) {
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

    let scenario = if has_existing_files() {
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
                        let finish_marker = generate_gitignore_preflight_file(codexize_entry)?;
                        debug_assert!(
                            finish_marker.exists(),
                            "deterministic preflight generation should create its marker eagerly"
                        );
                        run_git_init()?;
                        return Ok(PreflightOutcome::Continue);
                    }
                    run_git_init()?;
                    append_to_gitignore(codexize_entry)?;
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
                    append_to_gitignore(codexize_entry)?;
                    maybe_auto_commit_gitignore(|_| {});
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
        run_acp_install_modal(terminal, Scenario::CodexAcpMissing, install_codex_acp)?;
    }
    if crate::acp::should_offer_claude_acp_install() {
        run_acp_install_modal(terminal, Scenario::ClaudeAcpMissing, install_claude_acp)?;
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
mod tests {
    use super::*;
    use crate::state::test_fs_lock;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Modifier};
    use std::ffi::OsStr;

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

    #[test]
    fn claude_acp_install_root_uses_home_codexize_acp() {
        let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HOME", home.path());
        }

        let root = crate::acp::claude_acp_install_root();

        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }

        assert_eq!(root, home.path().join(".codexize").join("acp"));
    }

    #[test]
    fn preflight_exit_keys_request_normal_shutdown() {
        assert_eq!(
            classify_required_modal_key(KeyCode::Char('q')),
            ModalAction::Exit
        );
        assert_eq!(classify_required_modal_key(KeyCode::Esc), ModalAction::Exit);
        assert_eq!(
            classify_optional_modal_key(KeyCode::Char('q')),
            ModalAction::Skip
        );
        assert_eq!(classify_optional_modal_key(KeyCode::Esc), ModalAction::Skip);
    }

    fn render_preflight_buf(scenario: Scenario, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_preflight_modal(frame, scenario))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn raw_line_text(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>()
    }

    fn expected_dialog_rect(width: u16, height: u16, content_h: usize) -> Rect {
        let max_w = width.saturating_sub(4).max(1);
        let dialog_w = max_w.min(80).max(max_w.min(40));
        let dialog_h = ((content_h + 5) as u16).min(height.saturating_sub(4));
        Rect::new(
            (width.saturating_sub(dialog_w)) / 2,
            (height.saturating_sub(dialog_h)) / 2,
            dialog_w,
            dialog_h,
        )
    }

    fn scenario_body_line_count(scenario: Scenario, width: u16, height: u16) -> usize {
        let area = Rect::new(0, 0, width, height);
        let (_, body_copy, _) = preflight_modal_content(scenario);
        preflight_body_lines(area, body_copy).len()
    }

    #[test]
    fn preflight_modals_use_shared_visual_contract_for_each_scenario() {
        let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        for scenario in [
            Scenario::NoGitEmpty,
            Scenario::NoGitHasFiles,
            Scenario::GitExistsNotIgnored,
            Scenario::CodexAcpMissing,
            Scenario::ClaudeAcpMissing,
        ] {
            let width = 100;
            let height = 30;
            let buf = render_preflight_buf(scenario, width, height);
            let dialog = expected_dialog_rect(
                width,
                height,
                scenario_body_line_count(scenario, width, height),
            );

            let corner = &buf[(dialog.x, dialog.y)];
            assert_eq!(corner.symbol(), "┌");
            assert_eq!(corner.fg, Color::Yellow);
            assert!(corner.modifier.contains(Modifier::BOLD));

            for y in dialog.y..dialog.y + dialog.height {
                for x in dialog.x..dialog.x + dialog.width {
                    assert_eq!(buf[(x, y)].bg, Color::Black);
                }
            }

            assert!(
                raw_line_text(&buf, dialog.y + dialog.height - 3, width)
                    .trim()
                    .trim_matches('│')
                    .trim()
                    .is_empty(),
                "expected the reserved blank separator row above the preflight keymap"
            );

            let keymap_row = raw_line_text(&buf, dialog.y + dialog.height - 2, width);
            assert!(
                !keymap_row.trim().trim_matches('│').trim().is_empty(),
                "expected a visible keymap row for {scenario:?}"
            );

            for y in 0..height {
                for x in 0..width {
                    if (dialog.x..dialog.x + dialog.width).contains(&x)
                        && (dialog.y..dialog.y + dialog.height).contains(&y)
                    {
                        continue;
                    }
                    assert_ne!(
                        buf[(x, y)].bg,
                        Color::DarkGray,
                        "preflight should not draw the dashboard dim backdrop"
                    );
                }
            }
        }
    }

    #[test]
    fn preflight_modal_action_markers_keep_allowed_semantic_colors() {
        let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
        let width = 100;
        let height = 30;
        let buf = render_preflight_buf(Scenario::GitExistsNotIgnored, width, height);
        let dialog = expected_dialog_rect(
            width,
            height,
            scenario_body_line_count(Scenario::GitExistsNotIgnored, width, height),
        );
        let keymap_y = dialog.y + dialog.height - 2;
        let keymap_text = raw_line_text(&buf, keymap_y, width);

        let y_col = keymap_text.find("[Y]").expect("affirmative marker");
        let y_x = keymap_text[..y_col].chars().count() as u16 + 1;
        assert_eq!(buf[(y_x, keymap_y)].fg, Color::Green);

        let n_col = keymap_text.find("[N]").expect("secondary marker");
        let n_x = keymap_text[..n_col].chars().count() as u16 + 1;
        assert!(
            matches!(buf[(n_x, keymap_y)].fg, Color::White | Color::Gray),
            "secondary marker should stay in shared body colors"
        );

        let q_col = keymap_text.find("[Q]").expect("quit marker");
        let q_x = keymap_text[..q_col].chars().count() as u16 + 1;
        assert_eq!(buf[(q_x, keymap_y)].fg, Color::Red);
    }

    #[test]
    fn detect_ignored_accepts_required_directory_entry_before_dir_exists() {
        with_temp_dir(|| {
            git_cmd(&["init"]);
            fs::write(".gitignore", ".codexize/\n").unwrap();

            assert!(detect_ignored(Path::new(".codexize")));
        });
    }

    #[test]
    fn detect_ignored_accepts_required_entry_when_old_session_file_is_tracked() {
        with_temp_dir(|| {
            git_cmd(&["init"]);
            fs::write(".gitignore", ".codexize/\n").unwrap();
            fs::create_dir_all(".codexize/sessions/old/rounds/001").unwrap();
            fs::write(
                ".codexize/sessions/old/rounds/001/coder_summary.toml",
                "status = \"done\"\n",
            )
            .unwrap();
            git_cmd(&["add", ".gitignore"]);
            git_cmd(&[
                "add",
                "-f",
                ".codexize/sessions/old/rounds/001/coder_summary.toml",
            ]);

            assert!(detect_ignored(Path::new(".codexize")));
        });
    }

    #[test]
    fn gitignore_generation_is_deterministic_without_runtime_launch() {
        with_temp_dir(|| {
            fs::write("Cargo.toml", "[package]\nname = \"demo\"\n").unwrap();

            let fake_bin = Path::new("fake-bin");
            fs::create_dir_all(fake_bin).unwrap();
            let codex_log = Path::new("codex.log");
            write_fake_executable(
                &fake_bin.join("codex"),
                &format!(
                    "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                    codex_log.display()
                ),
            );

            let original_path = std::env::var_os("PATH");
            // SAFETY: serialized via test_fs_lock and restored below.
            unsafe {
                std::env::set_var("PATH", fake_bin);
            }

            let outcome =
                std::panic::catch_unwind(|| generate_gitignore_preflight_file(".codexize/"));

            unsafe {
                match original_path {
                    Some(value) => std::env::set_var("PATH", value),
                    None => std::env::remove_var("PATH"),
                }
            }

            let finish_marker = outcome
                .expect("gitignore generation should not panic")
                .expect("gitignore generation should succeed");
            let content = fs::read_to_string(".gitignore").expect("read generated gitignore");
            assert!(content.contains(".codexize/"));
            assert!(content.contains("target/"));
            assert!(
                finish_marker.exists(),
                "expected finish marker to be written"
            );
            assert!(
                !codex_log.exists(),
                "preflight gitignore generation must not launch agent CLIs"
            );
        });
    }

    fn git_cmd(args: &[&str]) {
        let status = Command::new("git").args(args).status().unwrap();
        assert!(
            status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
    }

    fn git_output(args: &[&str]) -> String {
        let output = Command::new("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn git_output_allow_failure(args: &[&str], env: &[(&str, &OsStr)]) -> (bool, String, String) {
        let mut cmd = Command::new("git");
        cmd.args(args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let output = cmd.output().unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }

    fn init_repo_with_head() {
        git_cmd(&["init"]);
        git_cmd(&["config", "user.name", "Test User"]);
        git_cmd(&["config", "user.email", "test@example.com"]);
        fs::write("README.md", "seed\n").unwrap();
        git_cmd(&["add", "README.md"]);
        git_cmd(&["commit", "-m", "seed"]);
    }

    fn write_fake_executable(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn gitignore_modal_clean_repo_auto_commits_with_fixed_subject() {
        with_temp_dir(|| {
            init_repo_with_head();
            append_to_gitignore(".codexize/").unwrap();
            maybe_auto_commit_gitignore(|_| {});

            assert_eq!(
                git_output(&["log", "-1", "--format=%s"]),
                GITIGNORE_AUTO_COMMIT_SUBJECT
            );
            assert_eq!(git_output(&["status", "--porcelain"]), "");
            let tracked = git_output(&["show", "--name-only", "--format=", "HEAD"]);
            assert_eq!(tracked, ".gitignore");
        });
    }

    #[test]
    fn gitignore_modal_staged_gitignore_still_auto_commits() {
        with_temp_dir(|| {
            init_repo_with_head();
            fs::write(".gitignore", "target/\nlogs/\n").unwrap();
            git_cmd(&["add", ".gitignore"]);
            git_cmd(&["commit", "-m", "add gitignore"]);
            fs::write(".gitignore", "target/\nlogs/\ncache/\n").unwrap();
            git_cmd(&["add", ".gitignore"]);
            append_to_gitignore(".codexize/").unwrap();
            maybe_auto_commit_gitignore(|_| {});

            assert_eq!(
                git_output(&["log", "-1", "--format=%s"]),
                GITIGNORE_AUTO_COMMIT_SUBJECT
            );
            assert_eq!(git_output(&["status", "--porcelain"]), "");
            let content = fs::read_to_string(".gitignore").unwrap();
            assert!(content.contains("target/"));
            assert!(content.contains(".codexize/"));
        });
    }

    #[test]
    fn gitignore_modal_dirty_repo_skips_auto_commit() {
        with_temp_dir(|| {
            init_repo_with_head();
            let previous_head = git_output(&["rev-parse", "HEAD"]);
            fs::write("README.md", "dirty\n").unwrap();
            append_to_gitignore(".codexize/").unwrap();
            maybe_auto_commit_gitignore(|_| {});

            assert_eq!(git_output(&["rev-parse", "HEAD"]), previous_head);
            let status = git_output(&["status", "--porcelain"]);
            assert!(status.contains(".gitignore"));
        });
    }

    #[test]
    fn gitignore_modal_only_codexize_changes_skips_auto_commit() {
        with_temp_dir(|| {
            init_repo_with_head();
            let previous_head = git_output(&["rev-parse", "HEAD"]);
            fs::create_dir(".codexize").unwrap();
            fs::write(".codexize/note.txt", "internal").unwrap();
            maybe_auto_commit_gitignore(|_| {});

            assert_eq!(git_output(&["rev-parse", "HEAD"]), previous_head);
        });
    }

    #[test]
    fn gitignore_modal_missing_identity_is_swallowed_and_warned() {
        with_temp_dir(|| {
            git_cmd(&["init"]);
            git_cmd(&["config", "user.name", ""]);
            git_cmd(&["config", "user.email", ""]);
            let fake_home = tempfile::TempDir::new().unwrap();
            let empty_global = fake_home.path().join("empty-gitconfig");
            fs::write(&empty_global, "").unwrap();

            let env = [
                ("HOME", fake_home.path().as_os_str()),
                ("XDG_CONFIG_HOME", fake_home.path().as_os_str()),
                ("GIT_CONFIG_GLOBAL", empty_global.as_os_str()),
                ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
            ];

            append_to_gitignore(".codexize/").unwrap();
            let mut warnings = Vec::new();
            maybe_auto_commit_gitignore(|w| warnings.push(w));

            let (head_ok, _stdout, _stderr) =
                git_output_allow_failure(&["rev-parse", "HEAD"], &env);
            assert!(
                !head_ok,
                "no commit should be created when identity is missing"
            );
            assert!(
                warnings.iter().any(|w| {
                    w.contains("identity")
                        || w.contains("user.email")
                        || w.contains("user.name")
                        || w.contains("unable to auto-detect email address")
                }),
                "expected identity warning, got: {warnings:?}"
            );
        });
    }
}
