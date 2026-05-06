//! Finish-stamp + git-state helpers used by the runner's exit-policy path.
//!
//! Owns the on-disk finish-stamp format, HEAD-stabilization polling, and the
//! pure validators (`validate_toml_artifacts`, `enforce_readonly_workspace_policy`).
//! Higher-level supervisors call into these primitives to record the outcome
//! of a managed ACP run.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

/// Finish stamp written by the runner-owned wrapper after every agent attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinishStamp {
    pub finished_at: String,
    pub exit_code: i32,
    pub head_before: String,
    pub head_after: String,
    pub head_state: String,
    #[serde(default)]
    pub signal_received: String,
    #[serde(default)]
    pub working_tree_clean: bool,
}

/// Atomic write of a finish stamp: write to a temp file in the same directory,
/// then rename into place.
pub fn write_finish_stamp(path: &Path, stamp: &FinishStamp) -> Result<()> {
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&dir)?;

    let tmp_path = dir.join(format!(".tmp.{}.toml", std::process::id()));
    let text = toml::to_string_pretty(stamp).context("failed to serialize finish stamp")?;
    fs::write(&tmp_path, text)
        .with_context(|| format!("failed to write temp stamp {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename stamp to {}", path.display()))?;
    Ok(())
}

/// Read and parse a finish stamp from disk.
pub fn read_finish_stamp(path: &Path) -> Result<FinishStamp> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read finish stamp {}", path.display()))?;
    let stamp: FinishStamp = toml::from_str(&text)
        .with_context(|| format!("failed to parse finish stamp {}", path.display()))?;
    Ok(stamp)
}

/// Default stabilization budget in milliseconds.
const DEFAULT_STAMP_STABILIZE_BUDGET_MS: u64 = 1500;
/// Default interval between HEAD reads in milliseconds.
const DEFAULT_STAMP_STABILIZE_INTERVAL_MS: u64 = 100;

/// Environment variable overrides for stabilization timing.
const ENV_STAMP_STABILIZE_MS: &str = "CODEXIZE_STAMP_STABILIZE_MS";
const ENV_STAMP_STABILIZE_INTERVAL_MS: &str = "CODEXIZE_STAMP_STABILIZE_INTERVAL_MS";

pub(super) fn git_rev_parse_head() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
}

pub(super) fn working_tree_clean() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout.is_empty())
        .unwrap_or(false)
}

pub(super) fn git_status_porcelain() -> Result<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status --porcelain")?;
    if !output.status.success() {
        bail!("git status --porcelain failed with exit {}", output.status);
    }
    String::from_utf8(output.stdout).context("git status --porcelain emitted non-UTF-8 output")
}

fn stamp_stabilize_budget() -> Duration {
    std::env::var(ENV_STAMP_STABILIZE_MS)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_STABILIZE_BUDGET_MS))
}

fn stamp_stabilize_interval() -> Duration {
    std::env::var(ENV_STAMP_STABILIZE_INTERVAL_MS)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_STABILIZE_INTERVAL_MS))
}

/// Wait for `git HEAD` to stabilise after a managed ACP run, returning the
/// final SHA plus a label describing whether the budget was exhausted.
///
/// Async-only: callers must drive this from a tokio runtime so the post-loop
/// supervisor finalisation owns the await rather than crossing a sync bridge.
pub(super) async fn wait_for_stable_head() -> (String, String) {
    let budget = stamp_stabilize_budget();
    let interval = stamp_stabilize_interval();
    let deadline = Instant::now() + budget;

    loop {
        let lock_path = Path::new(".git").join("index.lock");
        while tokio::fs::metadata(&lock_path).await.is_ok() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let first = git_rev_parse_head().unwrap_or_default();
        tokio::time::sleep(interval).await;
        let second = git_rev_parse_head().unwrap_or_default();
        if first == second {
            return (second, "stable".to_string());
        }
        if Instant::now() >= deadline {
            return (second, "unstable".to_string());
        }
    }
}

/// Compose a finish stamp from a completed managed-run outcome and persist it
/// to disk. The supervisor passes its `ManagedAcpOutcome` fields as primitives
/// so this module does not need to depend on the supervisor's types.
pub(super) async fn write_finish_stamp_for_outcome(
    stamp_path: &Path,
    head_before: String,
    exit_code: i32,
    signal_received: &str,
) -> Result<()> {
    let (head_after, head_state) = wait_for_stable_head().await;
    let stamp = FinishStamp {
        finished_at: chrono::Utc::now().to_rfc3339(),
        exit_code,
        head_before,
        head_after,
        head_state,
        signal_received: signal_received.to_string(),
        working_tree_clean: working_tree_clean(),
    };
    write_finish_stamp(stamp_path, &stamp)
}

/// Verify the workspace was untouched by an ACP run when the launch policy
/// requires it (read-only enforcement). The supervisor decides whether the
/// policy applies and supplies the pre-run snapshot of git state.
pub(super) fn enforce_readonly_workspace_policy(
    enforce: bool,
    head_before: &str,
    git_status_before: Option<&str>,
) -> Result<()> {
    if !enforce {
        return Ok(());
    }

    let head_after = git_rev_parse_head().unwrap_or_default();
    if head_after != head_before {
        bail!(
            "ACP launch violated read-only workspace policy: HEAD changed from {head_before} to {head_after}"
        );
    }

    let Some(git_status_before) = git_status_before else {
        bail!("ACP launch violated read-only workspace policy: missing pre-run git status");
    };
    let git_status_after = git_status_porcelain()?;
    if git_status_after != git_status_before {
        bail!(
            "ACP launch violated read-only workspace policy: git status changed from {:?} to {:?}",
            git_status_before,
            git_status_after
        );
    }

    Ok(())
}

/// Validate that all required TOML artifacts exist and are parseable.
/// Missing or malformed artifacts signal an incomplete agent turn; the
/// orchestrator should retry the agent execution phase.
pub fn validate_toml_artifacts(paths: &[&Path]) -> Result<()> {
    let mut errors = Vec::new();
    for path in paths {
        if !path.exists() {
            errors.push(format!("missing: {}", path.display()));
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let text = fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if let Err(e) = toml::from_str::<toml::Value>(&text) {
                errors.push(format!("malformed TOML in {}: {e}", path.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(
            "incomplete agent turn — artifact validation failed:\n{}",
            errors.join("\n")
        )
    }
}
