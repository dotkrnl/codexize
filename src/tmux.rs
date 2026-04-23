use anyhow::{Context, Result, bail};
use std::{env, process::Command};

#[derive(Debug, Clone)]
pub struct TmuxContext {
    pub session_name: String,
    pub window_index: String,
    pub window_name: String,
}

pub fn current_context() -> Result<TmuxContext> {
    if env::var_os("TMUX").is_none() {
        bail!("codexize must be started inside tmux");
    }

    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#{session_name}\t#{window_index}\t#{window_name}",
        ])
        .output()
        .context("failed to query tmux context")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.is_empty() {
            bail!("tmux context query failed");
        }
        bail!("tmux context query failed: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout).context("tmux returned non-utf8 context")?;
    let mut parts = stdout.trim_end().splitn(3, '\t');

    let session_name = parts.next().unwrap_or_default().to_owned();
    let window_index = parts.next().unwrap_or_default().to_owned();
    let window_name = parts.next().unwrap_or_default().to_owned();

    if session_name.is_empty() || window_index.is_empty() {
        bail!("tmux returned incomplete context");
    }

    Ok(TmuxContext {
        session_name,
        window_index,
        window_name,
    })
}

pub fn window_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["list-windows", "-F", "#{window_name}"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line == name)
        })
        .unwrap_or(false)
}

pub fn create_window(name: &str, command: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["new-window", "-n", name, command])
        .status()
        .context("failed to run tmux new-window")?;

    if !status.success() {
        bail!("tmux new-window failed");
    }

    Ok(())
}

pub fn switch_to_window(name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["select-window", "-t", name])
        .status()
        .context("failed to run tmux select-window")?;

    if !status.success() {
        bail!("tmux select-window failed");
    }

    Ok(())
}
