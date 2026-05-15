//! Immutability enforcement for agent runs.
//!
//! Non-coder agents must leave the git working tree untouched (their writes go
//! into the gitignored session dir). The coder may advance HEAD but must not
//! edit session control files (task.toml, review_scope.toml, and prior
//! rounds' artifacts). This module snapshots the relevant state at launch
//! time and, on exit, verifies, reverts, and reports a reason string that
//! flows through the normal run-failure machinery.
use crate::app::Reason;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
const SNAPSHOT_FILE: &str = "snapshot.toml";
/// Decided at run-launch time and persisted on the snapshot so finalization
/// and resume paths cannot lose the choice. `AutoReset` keeps today's
/// behavior (fail closed; `git reset --hard` on HEAD-advance). `AskOperator`
/// is reserved for interactive non-coder runs: verify reports a pending
/// decision instead of resetting, and the operator chooses reset vs keep.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuardMode {
    #[default]
    AutoReset,
    AskOperator,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct Snapshot {
    /// git HEAD at capture time (full SHA). Empty if git was unavailable.
    #[serde(default)]
    pub head: String,
    /// Output of `git status --porcelain` at capture time. Empty if git failed.
    #[serde(default)]
    pub git_status: String,
    /// path → file bytes, for files the agent must not modify. UTF-8 only
    /// (the control files are all text TOML/markdown).
    #[serde(default)]
    pub control_files: BTreeMap<String, String>,
    /// If the working tree was dirty at capture time, we stash those changes
    /// so the agent starts from a clean baseline; this is the stash message
    /// used to locate and pop the entry during verify.
    #[serde(default)]
    pub baseline_stash: Option<String>,
    /// How the guard should react to a HEAD-advance violation.
    pub mode: GuardMode,
    /// Reviewer-only working-tree baseline when dirty changes are in scope.
    #[serde(default)]
    pub working_tree_baseline: Option<String>,
}
/// Outcome of verifying a snapshot. Three arms so callers cannot
/// accidentally treat a pending operator decision as a hard error.
#[derive(Debug, Clone)]
pub(crate) enum VerifyResult {
    /// No protocol violation. Advisory `warnings` may still be present.
    Ok { warnings: Vec<String> },
    /// Hard violation that the guard already reacted to (reset, restored
    /// control files, etc.). The run should fail with `reason`.
    HardError {
        reason: String,
        warnings: Vec<String>,
    },
    /// Interactive non-coder run advanced HEAD. The guard did **not** reset;
    /// the operator must choose reset vs keep.
    PendingDecision {
        captured_head: String,
        current_head: String,
        warnings: Vec<String>,
    },
}
fn git_stdout(args: &[&str]) -> Option<String> {
    #[cfg(test)]
    let _guard = crate::state::test_fs_lock().lock();
    let output = std::process::Command::new("git")
        .args(args)
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}
fn git_head() -> Option<String> {
    #[cfg(test)]
    let _guard = crate::state::test_fs_lock().lock();
    let trimmed = git_stdout(&["rev-parse", "HEAD"])?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}
pub(crate) fn git_status_dirty() -> bool {
    git_status().is_some_and(|s| !s.trim().is_empty())
}
fn git_status() -> Option<String> {
    #[cfg(test)]
    let _guard = crate::state::test_fs_lock().lock();
    git_stdout(&["status", "--porcelain"])
}
fn git_diff_head() -> Option<String> {
    git_stdout(&["diff", "HEAD"])
}
fn git_working_tree_baseline() -> Option<String> {
    let diff = git_diff_head()?;
    let status = git_status()?;
    Some(format!("diff:\n{diff}\nstatus:\n{status}"))
}
fn write_snapshot(snapshot_dir: &Path, snap: &Snapshot) -> std::io::Result<()> {
    std::fs::create_dir_all(snapshot_dir)?;
    let text = toml::to_string(snap)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(snapshot_dir.join(SNAPSHOT_FILE), text)
}
fn read_snapshot(snapshot_dir: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(snapshot_dir.join(SNAPSHOT_FILE)).ok()?;
    toml::from_str(&text).ok()
}
/// Capture a snapshot for a non-coder agent. Records HEAD and working-tree
/// status so `verify_non_coder` can detect changes and emit warnings.
/// `mode` controls how a HEAD-advance violation is handled at verify time.
pub(crate) fn capture_non_coder(
    snapshot_dir: &Path,
    _stage_tag: &str,
    mode: GuardMode,
    track_working_tree: bool,
) -> std::io::Result<()> {
    let snap = Snapshot {
        head: git_head().unwrap_or_default(),
        git_status: git_status().unwrap_or_default(),
        control_files: BTreeMap::new(),
        baseline_stash: None,
        mode,
        working_tree_baseline: track_working_tree.then(git_working_tree_baseline).flatten(),
    };
    write_snapshot(snapshot_dir, &snap)
}
/// Gather the set of control files the coder must not modify for this round.
/// Includes the current round's task.toml / review_scope.toml and every
/// file under prior rounds' directories.
fn coder_control_paths(session_dir: &Path, round: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let current = session_dir.join("rounds").join(format!("{round:03}"));
    for name in ["task.toml", "review_scope.toml"] {
        let p = current.join(name);
        if p.is_file() {
            out.push(p);
        }
    }
    let rounds_root = session_dir.join("rounds");
    if let Ok(entries) = std::fs::read_dir(&rounds_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(r) = name.parse::<u32>() else { continue };
            if r >= round {
                continue;
            }
            if let Ok(inner) = std::fs::read_dir(&path) {
                for f in inner.flatten() {
                    let p = f.path();
                    if p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}
pub(crate) fn capture_coder(
    snapshot_dir: &Path,
    session_dir: &Path,
    round: u32,
) -> std::io::Result<()> {
    let mut control_files = BTreeMap::new();
    for p in coder_control_paths(session_dir, round) {
        if let Ok(text) = std::fs::read_to_string(&p) {
            control_files.insert(p.display().to_string(), text);
        }
    }
    let snap = Snapshot {
        head: git_head().unwrap_or_default(),
        git_status: String::new(),
        control_files,
        baseline_stash: None,
        mode: GuardMode::default(),
        working_tree_baseline: None,
    };
    write_snapshot(snapshot_dir, &snap)
}
/// Verify the snapshot. Returns a typed three-arm result so callers cannot
/// accidentally treat a pending operator decision as a hard error.
pub(crate) fn verify(snapshot_dir: &Path, stage: &str) -> VerifyResult {
    let Some(snap) = read_snapshot(snapshot_dir) else {
        return VerifyResult::Ok { warnings: vec![] };
    };
    // Simplifier shares the coder's verify rules: HEAD may advance via
    // `refactor:`/`style:` commits, but session control files must stay put.
    if stage == "coder" || stage == "simplifier" {
        verify_coder(&snap)
    } else {
        verify_non_coder(&snap)
    }
}
/// Run `git reset --hard <captured_head>` so an operator-driven reset can
/// share the exact reset path used by `AutoReset` mode without having to
/// re-read the snapshot.
pub(crate) fn reset_hard_to(captured_head: &str) -> bool {
    if captured_head.is_empty() {
        return false;
    }
    #[cfg(test)]
    let _guard = crate::state::test_fs_lock().lock();
    std::process::Command::new("git")
        .args(["reset", "--hard", captured_head])
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
fn verify_non_coder(snap: &Snapshot) -> VerifyResult {
    let mut warnings = Vec::new();
    if !snap.git_status.trim().is_empty() {
        warnings.push("working tree was dirty before agent launch".to_string());
    }
    let current_status = git_status().unwrap_or_default();
    let current_head = git_head().unwrap_or_default();
    let head_changed =
        !snap.head.is_empty() && !current_head.is_empty() && current_head != snap.head;
    if current_status.trim() != snap.git_status.trim() {
        warnings.push("non-coder agent modified working tree".to_string());
    }
    if head_changed {
        return match snap.mode {
            GuardMode::AutoReset => {
                let _ = reset_hard_to(&snap.head);
                VerifyResult::HardError {
                    reason: Reason::ForbiddenHeadAdvance.to_string(),
                    warnings,
                }
            }
            GuardMode::AskOperator => VerifyResult::PendingDecision {
                captured_head: snap.head.clone(),
                current_head,
                warnings,
            },
        };
    }
    if let Some(expected) = &snap.working_tree_baseline {
        let current = git_working_tree_baseline().unwrap_or_default();
        if &current != expected {
            return VerifyResult::HardError {
                reason: Reason::ReviewerModifiedWorkingTree.to_string(),
                warnings,
            };
        }
    }
    VerifyResult::Ok { warnings }
}
/// Construct a `Snapshot` for testing without running real git commands.
#[cfg(test)]
fn test_snapshot(head: &str, git_status: &str) -> Snapshot {
    Snapshot {
        head: head.to_string(),
        git_status: git_status.to_string(),
        control_files: BTreeMap::new(),
        baseline_stash: None,
        mode: GuardMode::AutoReset,
        working_tree_baseline: None,
    }
}
fn verify_coder(snap: &Snapshot) -> VerifyResult {
    let mut violated = Vec::new();
    for (path, expected) in &snap.control_files {
        let actual = std::fs::read_to_string(path).ok();
        if actual.as_deref() != Some(expected.as_str()) {
            let _ = std::fs::write(path, expected);
            violated.push(path.clone());
        }
    }
    if violated.is_empty() {
        VerifyResult::Ok { warnings: vec![] }
    } else {
        VerifyResult::HardError {
            reason: Reason::ForbiddenControlEdit(violated.join(", ")).to_string(),
            warnings: vec![],
        }
    }
}
#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
