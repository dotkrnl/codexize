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
    fn build_command(&self, run: &AgentRun) -> Vec<String>;
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

    fn build_command(&self, run: &AgentRun) -> Vec<String> {
        vec![
            "codex".to_string(),
            "--model".to_string(),
            run.model.clone(),
            "--quiet".to_string(),
            format!("@{}", run.prompt_path.display()),
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

    let agent_cmd = adapter.build_command(run);
    let artifact_args: Vec<String> = run
        .artifact_paths
        .iter()
        .flat_map(|p| ["--artifact".to_string(), p.display().to_string()])
        .collect();

    // Full command run inside the tmux window:
    //   codexize agent-run --run-id X --phase Y --role Z \
    //     --artifact <path> ... \
    //     -- <adapter command>
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
    wrapper.extend(agent_cmd);

    let shell_cmd = shell_escape_join(&wrapper);

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

fn shell_escape_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.chars().all(|c| c.is_alphanumeric() || "-_./@=:".contains(c)) {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
