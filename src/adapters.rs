use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub struct AgentRun {
    pub run_id: String,
    pub phase: String,
    pub role: String,
    pub model: String,
    pub prompt_path: PathBuf,
    pub artifact_paths: Vec<PathBuf>,
}

pub struct CodexAdapter;

impl CodexAdapter {
    pub fn detect(&self) -> bool {
        Command::new("codex")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Launch an interactive codex session in a new named tmux window.
///
/// The prompt file is injected as the initial message via `$(cat ...)` shell
/// expansion so codex gets context immediately while still running fully
/// interactively (PTY intact, user can type back).
///
/// Artifact validation is the caller's responsibility (poll_agent_window).
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    adapter: &CodexAdapter,
    switch: bool,
) -> Result<()> {
    if !adapter.detect() {
        bail!("codex CLI not found — install it first");
    }

    let prompt_path = shell_escape(run.prompt_path.to_string_lossy().as_ref());

    // codex -m <model> "$(cat <prompt>)"
    // The shell expands the prompt file content as the initial message;
    // the session is then fully interactive.
    let shell_cmd = format!(
        r#"codex -m {model} "$(cat {prompt_path})" || {{ echo; echo '--- codex exited, press enter to close ---'; read; }}"#,
        model = shell_escape(&run.model),
        prompt_path = prompt_path,
    );

    let status = Command::new("tmux")
        .args(["new-window", "-n", window_name, &shell_cmd])
        .status()
        .context("failed to create tmux window")?;

    if !status.success() {
        bail!("tmux new-window failed");
    }

    if switch {
        let status = Command::new("tmux")
            .args(["select-window", "-t", window_name])
            .status()
            .context("failed to switch to agent window")?;

        if !status.success() {
            bail!("tmux select-window failed");
        }
    }

    Ok(())
}

fn shell_escape(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "-_./@=:".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
