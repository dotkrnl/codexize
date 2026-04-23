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

pub trait AgentAdapter: Send + Sync {
    fn detect(&self) -> bool;
    fn interactive_command(&self, model: &str, prompt_path: &str) -> String;
    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String;
}

// ── Codex ────────────────────────────────────────────────────────────────────

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn detect(&self) -> bool {
        Command::new("codex").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"codex --yolo -m {model} "$(cat {prompt_path})""#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"codex exec --yolo -m {model} - < {prompt_path}"#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
        )
    }
}

// ── Claude ───────────────────────────────────────────────────────────────────

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn detect(&self) -> bool {
        Command::new("claude").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"claude --dangerously-skip-permissions --model {model} "$(cat {prompt_path})""#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String {
        // stream-json + --include-partial-messages emits live token/tool/thinking
        // events. The jq filter formats each type with a coloured marker so the
        // operator can distinguish text, thinking, and tool use in real time.
        // ANSI codes use jq's  Unicode escape (ESC byte).
        let filter = r##"if .type=="content_block_start" then (if .content_block.type=="thinking" then "\n[2;35m💭 thinking[0m\n[2;35m" elif .content_block.type=="tool_use" then "\n[1;33m🔧 \(.content_block.name)[0m\n[33m" elif .content_block.type=="text" then "[0m\n" else "" end) elif .type=="content_block_delta" then (.delta.text // .delta.thinking // .delta.partial_json // empty) elif .type=="content_block_stop" then "[0m\n" elif .type=="message_stop" then "\n" else empty end"##;
        format!(
            r#"claude --dangerously-skip-permissions --print --output-format stream-json --include-partial-messages --verbose --model {model} < {prompt_path} | jq -jr --unbuffered '{filter}'"#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
            filter = filter,
        )
    }
}

// ── Kimi ─────────────────────────────────────────────────────────────────────

pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn detect(&self) -> bool {
        Command::new("kimi").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, _model: &str, prompt_path: &str) -> String {
        format!(
            r#"kimi --yolo -p "$(cat {prompt_path})""#,
            prompt_path = shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, _model: &str, prompt_path: &str) -> String {
        format!(
            r#"kimi --yolo --print < {prompt_path}"#,
            prompt_path = shell_escape(prompt_path),
        )
    }
}

// ── Gemini ───────────────────────────────────────────────────────────────────

pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn detect(&self) -> bool {
        Command::new("gemini").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, model: &str, prompt_path: &str) -> String {
        // -i / --prompt-interactive explicitly executes the prompt and
        // continues in interactive mode (vs a bare positional which could
        // be interpreted as a subcommand name in some shells)
        format!(
            r#"gemini --yolo -m {model} -i "$(cat {prompt_path})""#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"gemini --yolo -m {model} -p "$(cat {prompt_path})""#,
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
