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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_context_errors_when_tmux_env_is_unset() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = env::var_os("TMUX");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            env::remove_var("TMUX");
        }
        let result = current_context();
        unsafe {
            if let Some(value) = original {
                env::set_var("TMUX", value);
            }
        }
        let err = result.expect_err("missing TMUX must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("must be started inside tmux"),
            "missing-tmux error context: {msg}"
        );
    }

    #[test]
    fn window_exists_returns_false_for_random_name() {
        // Use a UUID-ish suffix so the lookup cannot accidentally match a
        // real window on the developer's tmux server.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("__codexize_test_window_does_not_exist_{nonce}__");
        assert!(!window_exists(&name));
    }
}
