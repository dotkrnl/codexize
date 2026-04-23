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

pub trait AgentAdapter {
    fn detect(&self) -> bool;
    // Returns (program, args) — prompt is delivered via stdin redirect, not args
    fn exec_args(&self, run: &AgentRun) -> Vec<String>;
}

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn detect(&self) -> bool {
        Command::new("codex")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn exec_args(&self, run: &AgentRun) -> Vec<String> {
        // codex exec -m <model> -
        // The `-` tells codex to read the prompt from stdin.
        // The caller redirects stdin from the prompt file.
        vec![
            "codex".to_string(),
            "exec".to_string(),
            "-m".to_string(),
            run.model.clone(),
            "-".to_string(),
        ]
    }
}

pub fn launch_in_window(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
) -> Result<()> {
    if !adapter.detect() {
        bail!("codex CLI not found — install it first");
    }

    let exec_args = adapter.exec_args(run);
    let artifact_args: Vec<String> = run
        .artifact_paths
        .iter()
        .flat_map(|p| ["--artifact".to_string(), p.display().to_string()])
        .collect();

    // Assemble the wrapper invocation
    let mut wrapper: Vec<String> = vec![
        "codexize".to_string(),
        "agent-run".to_string(),
        "--run-id".to_string(),
        run.run_id.clone(),
        "--phase".to_string(),
        run.phase.clone(),
        "--role".to_string(),
        run.role.clone(),
    ];
    wrapper.extend(artifact_args);
    wrapper.push("--".to_string());
    wrapper.extend(exec_args);

    // Build the full shell command with stdin redirect from the prompt file
    // and a read-pause on failure so the window stays visible for diagnosis.
    let wrapper_str = shell_escape_join(&wrapper);
    let prompt_path = shell_escape(run.prompt_path.to_string_lossy().as_ref());
    let shell_cmd = format!(
        "{wrapper_str} < {prompt_path} || {{ echo; echo '--- agent-run failed, press enter to close ---'; read; }}"
    );

    let status = Command::new("tmux")
        .args(["new-window", "-n", window_name, &shell_cmd])
        .status()
        .context("failed to create tmux window")?;

    if !status.success() {
        bail!("tmux new-window failed");
    }

    let status = Command::new("tmux")
        .args(["select-window", "-t", window_name])
        .status()
        .context("failed to switch to tmux window")?;

    if !status.success() {
        bail!("tmux select-window failed");
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

fn shell_escape_join(args: &[String]) -> String {
    args.iter().map(|a| shell_escape(a)).collect::<Vec<_>>().join(" ")
}
