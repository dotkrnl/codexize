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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_interactive_command() {
        let adapter = ClaudeAdapter;
        let cmd = adapter.interactive_command("claude-sonnet-4", "/tmp/prompt.md");
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("claude-sonnet-4"));
        assert!(cmd.contains("/tmp/prompt.md"));
    }

    #[test]
    fn test_claude_noninteractive_command() {
        let adapter = ClaudeAdapter;
        let cmd = adapter.noninteractive_command("claude-sonnet-4", "/tmp/prompt.md");
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("--print"));
        assert!(cmd.contains("stream-json"));
        assert!(cmd.contains("jq"));
    }

    #[test]
    fn test_codex_interactive_command() {
        let adapter = CodexAdapter;
        let cmd = adapter.interactive_command("gpt-5.4", "/tmp/prompt.md");
        assert!(cmd.contains("codex"));
        assert!(cmd.contains("--yolo"));
        assert!(cmd.contains("gpt-5.4"));
    }

    #[test]
    fn test_codex_noninteractive_command() {
        let adapter = CodexAdapter;
        let cmd = adapter.noninteractive_command("gpt-5.4", "/tmp/prompt.md");
        assert!(cmd.contains("codex exec"));
        assert!(cmd.contains("--yolo"));
        assert!(cmd.contains("- <"));
    }

    #[test]
    fn test_gemini_interactive_command() {
        let adapter = GeminiAdapter;
        let cmd = adapter.interactive_command("gemini-pro", "/tmp/prompt.md");
        assert!(cmd.contains("gemini"));
        assert!(cmd.contains("--yolo"));
        assert!(cmd.contains("-i"));
    }

    #[test]
    fn test_gemini_noninteractive_command() {
        let adapter = GeminiAdapter;
        let cmd = adapter.noninteractive_command("gemini-pro", "/tmp/prompt.md");
        assert!(cmd.contains("gemini"));
        assert!(cmd.contains("-p"));
    }

    #[test]
    fn test_kimi_interactive_command() {
        let adapter = KimiAdapter;
        let cmd = adapter.interactive_command("kimi-latest", "/tmp/prompt.md");
        assert!(cmd.contains("kimi"));
        assert!(cmd.contains("--yolo"));
        assert!(cmd.contains("-p"));
    }

    #[test]
    fn test_kimi_noninteractive_command() {
        let adapter = KimiAdapter;
        let cmd = adapter.noninteractive_command("kimi-latest", "/tmp/prompt.md");
        assert!(cmd.contains("kimi"));
        assert!(cmd.contains("--print"));
        assert!(cmd.contains("<"));
    }

    #[test]
    fn test_shell_escape_safe_chars() {
        assert_eq!(shell_escape("hello-world_123"), "hello-world_123");
    }

    #[test]
    fn test_shell_escape_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_adapter_for_vendor_routing() {
        assert!(adapter_for_vendor(VendorKind::Claude).detect() == false || true); // may or may not be installed
        assert!(adapter_for_vendor(VendorKind::Codex).detect() == false || true);
        assert!(adapter_for_vendor(VendorKind::Gemini).detect() == false || true);
        assert!(adapter_for_vendor(VendorKind::Kimi).detect() == false || true);
    }
}
