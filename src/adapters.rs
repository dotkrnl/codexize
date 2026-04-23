use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

use crate::selection::VendorKind;

pub struct AgentRun {
    pub run_id: String,
    pub phase: String,
    pub role: String,
    pub model: String,
    pub prompt_path: PathBuf,
    pub artifact_paths: Vec<PathBuf>,
}

/// An adapter builds the shell command fragment that runs the agent interactively.
/// `model` is the selected model name; some adapters ignore it if the CLI
/// does not support per-invocation model selection.
pub trait AgentAdapter: Send + Sync {
    fn detect(&self) -> bool;
    fn window_command(&self, model: &str, prompt_path: &str) -> String;
}

// ── Codex ────────────────────────────────────────────────────────────────────

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn detect(&self) -> bool {
        Command::new("codex")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn window_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"codex -m {model} "$(cat {prompt_path})""#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
        )
    }
}

// ── Claude ───────────────────────────────────────────────────────────────────

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn detect(&self) -> bool {
        Command::new("claude")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn window_command(&self, _model: &str, prompt_path: &str) -> String {
        // Claude Code does not accept a per-invocation model flag in interactive
        // mode; it uses whatever is configured. The model selection still
        // determines which vendor is chosen.
        format!(
            r#"claude --dangerously-skip-permissions "$(cat {prompt_path})""#,
            prompt_path = shell_escape(prompt_path),
        )
    }
}

// ── Kimi ─────────────────────────────────────────────────────────────────────

pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn detect(&self) -> bool {
        Command::new("kimi")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn window_command(&self, _model: &str, prompt_path: &str) -> String {
        format!(
            r#"kimi --yolo -p "$(cat {prompt_path})""#,
            prompt_path = shell_escape(prompt_path),
        )
    }
}

// ── Gemini ───────────────────────────────────────────────────────────────────

pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn detect(&self) -> bool {
        Command::new("gemini")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn window_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"gemini --yolo -m {model} "$(cat {prompt_path})""#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
        )
    }
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

pub fn adapter_for_vendor(vendor: VendorKind) -> Box<dyn AgentAdapter> {
    match vendor {
        VendorKind::Codex => Box::new(CodexAdapter),
        VendorKind::Claude => Box::new(ClaudeAdapter),
        VendorKind::Kimi => Box::new(KimiAdapter),
        VendorKind::Gemini => Box::new(GeminiAdapter),
    }
}

// ── Launch ───────────────────────────────────────────────────────────────────

/// Create a named tmux window running the agent interactively.
/// The prompt file is injected via `$(cat <path>)` shell expansion.
/// `switch`: if true, switch the operator to the new window immediately.
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    switch: bool,
) -> Result<()> {
    if !adapter.detect() {
        bail!("agent CLI not found — install it first");
    }

    let prompt_path = run.prompt_path.to_string_lossy();
    let agent_cmd = adapter.window_command(&run.model, &prompt_path);

    let shell_cmd = format!(
        r#"{agent_cmd} || {{ echo; echo '--- agent exited, press enter to close ---'; read; }}"#,
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
