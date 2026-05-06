//! Backend half of the preflight flow.
//!
//! Pure(-ish) backend predicates and side-effecting installers used by
//! `ui::preflight`. The backend reports facts and performs the chosen
//! filesystem/process actions; the UI layer renders modals and routes
//! operator decisions back here.

use anyhow::{Context, Result};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

/// Outcome of the preflight flow as observed by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightOutcome {
    Continue,
    Exit,
}

/// Backend-detected scenario the runtime should surface to the operator.
///
/// The variants are UI-neutral facts derived from filesystem/process probes;
/// `ui::preflight` decides how to render and what keymap to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    NoGitEmpty,
    NoGitHasFiles,
    GitExistsNotIgnored,
    CodexAcpMissing,
    ClaudeAcpMissing,
}

pub const GITIGNORE_AUTO_COMMIT_SUBJECT: &str = "chore: ignore .codexize session data";

pub fn detect_git() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn detect_ignored(root: &Path) -> bool {
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

pub fn has_existing_files() -> bool {
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

pub fn append_to_gitignore(entry: &str) -> Result<()> {
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

pub fn maybe_auto_commit_gitignore<F>(mut warn: F)
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

pub fn run_git_init() -> Result<()> {
    let status = Command::new("git")
        .arg("init")
        .status()
        .context("failed to run git init")?;
    if !status.success() {
        anyhow::bail!("git init failed with status {}", status);
    }
    Ok(())
}

pub fn install_claude_acp() -> Result<()> {
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

pub fn install_codex_acp() -> Result<()> {
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

pub fn generate_heuristic_gitignore(codexize_entry: &str) -> String {
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

pub fn generate_gitignore_preflight_file(codexize_entry: &str) -> Result<std::path::PathBuf> {
    let finish_marker =
        std::env::temp_dir().join(format!("codexize-gitignore-{}.done", std::process::id()));
    // Preflight intentionally stays deterministic here instead of opening an
    // ACP session before codexize has created session state for a real run.
    let content = generate_heuristic_gitignore(codexize_entry);
    fs::write(".gitignore", content).context("failed to write .gitignore")?;
    fs::write(&finish_marker, "").context("failed to write finish marker")?;
    Ok(finish_marker)
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
