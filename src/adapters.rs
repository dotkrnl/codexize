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
    /// Interactive command — agent keeps the session open for user input.
    fn interactive_command(&self, model: &str, prompt_path: &str) -> String;
    /// Non-interactive command — agent reads prompt, writes artifact, and exits.
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
        // codex exec reads prompt from stdin and exits when done.
        // --yolo suppresses permission prompts so it can run headless.
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
        // Claude's --print text mode does not emit partial chunks — only the
        // final response. stream-json + --include-partial-messages is the
        // only way to get live token streaming; pipe through jq to recover
        // plain text. Reading the prompt from stdin avoids ARG_MAX issues.
        format!(
            r#"claude --dangerously-skip-permissions --print --output-format stream-json --include-partial-messages --verbose --model {model} < {prompt_path} | jq -jr --unbuffered '(.delta.text // .message.content[0].text // empty)'"#,
            model = shell_escape(model),
            prompt_path = shell_escape(prompt_path),
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
        // Kimi's --print mode requires input via stdin, not -p
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
        format!(
            r#"gemini --yolo -m {model} "$(cat {prompt_path})""#,
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

/// Create a named tmux window running the agent interactively.
/// `switch`: if true, switch the operator to the new window immediately.
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    switch: bool,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.interactive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, switch)
}

/// Create a named tmux window running the agent non-interactively.
/// The window stays open after exit so the user can read the output.
/// Never switches focus.
pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.noninteractive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, false)
}

fn launch_in_window(
    window_name: &str,
    agent_cmd: &str,
    adapter: &dyn AgentAdapter,
    switch: bool,
) -> Result<()> {
    if !adapter.detect() {
        bail!("agent CLI not found — install it first");
    }

    // Echo what's starting so the user has immediate feedback even if the
    // agent takes a while before producing its first output line.
    let shell_cmd = format!(
        r#"printf '\033[1;36m>>> starting %s...\033[0m\n\n' {name}; {agent_cmd}; echo; echo '--- done, press enter to close ---'; read"#,
        name = shell_escape(window_name),
    );

    // tmux new-window switches to the new window by default — add -d to
    // keep the operator focused where they are when switch=false
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
