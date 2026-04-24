use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::selection::VendorKind;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use kimi::KimiAdapter;

pub struct AgentRun {
    pub model: String,
    pub prompt_path: PathBuf,
}

pub trait AgentAdapter: Send + Sync {
    fn detect(&self) -> bool;
    fn interactive_command(&self, model: &str, prompt_path: &str) -> String;
    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String;
}

pub fn adapter_for_vendor(vendor: VendorKind) -> Box<dyn AgentAdapter> {
    match vendor {
        VendorKind::Codex => Box::new(CodexAdapter),
        VendorKind::Claude => Box::new(ClaudeAdapter),
        VendorKind::Kimi => Box::new(KimiAdapter),
        VendorKind::Gemini => Box::new(GeminiAdapter),
    }
}

pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    switch: bool,
    status_path: &Path,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.interactive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, switch, status_path)
}

pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    status_path: &Path,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.noninteractive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, false, status_path)
}

fn launch_in_window(
    window_name: &str,
    agent_cmd: &str,
    adapter: &dyn AgentAdapter,
    switch: bool,
    status_path: &Path,
) -> Result<()> {
    if !adapter.detect() {
        bail!("agent CLI not found — install it first");
    }

    let status_dir = status_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy()
        .into_owned();
    let status_path = status_path.to_string_lossy().into_owned();
    let shell_cmd = format!(
        r#"mkdir -p {status_dir}; trap 'status=$?; printf "%s" "$status" > {status_path}' EXIT HUP INT TERM; printf '\033[1;36m>>> starting %s...\033[0m\n\n' {name}; {agent_cmd}"#,
        status_dir = shell_escape(&status_dir),
        status_path = shell_escape(&status_path),
        name = shell_escape(window_name),
    );

    let args: Vec<&str> = if switch {
        vec!["new-window", "-n", window_name, &shell_cmd]
    } else {
        vec!["new-window", "-d", "-n", window_name, &shell_cmd]
    };
    let status = Command::new("tmux")
        .args(&args)
        .status()
        .context("failed to create tmux window")?;

    if !status.success() {
        bail!("tmux new-window failed");
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
