use anyhow::{Context, Result, bail};
use std::path::PathBuf;
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
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.interactive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, switch, /* wait_on_exit */ true)
}

pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.noninteractive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, false, /* wait_on_exit */ false)
}

fn launch_in_window(
    window_name: &str,
    agent_cmd: &str,
    adapter: &dyn AgentAdapter,
    switch: bool,
    wait_on_exit: bool,
) -> Result<()> {
    if !adapter.detect() {
        bail!("agent CLI not found — install it first");
    }

    // Non-interactive runs close the window as soon as the agent exits so
    // poll_agent_window sees the exit promptly. Interactive runs pause on
    // exit so the user can read any final messages.
    let tail = if wait_on_exit {
        r#"; echo; echo '--- done, press enter to close ---'; read"#
    } else {
        ""
    };
    let shell_cmd = format!(
        r#"printf '\033[1;36m>>> starting %s...\033[0m\n\n' {name}; {agent_cmd}{tail}"#,
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
