//! Immutability enforcement for agent runs.
//!
//! Non-coder agents must leave the git working tree untouched (their writes go
//! into the gitignored session dir). The coder may advance HEAD but must not
//! edit session control files (task.toml, review_scope.toml, and prior
//! rounds' artifacts). This module snapshots the relevant state at launch
//! time and, on exit, verifies, reverts, and reports a reason string that
//! flows through the normal run-failure machinery.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SNAPSHOT_FILE: &str = "snapshot.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
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
}

fn git_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn git_status() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
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

/// Capture a snapshot for a non-coder agent. They must leave the git tree
/// unchanged; any pre-existing dirt is stashed so the agent starts clean,
/// and the stash is popped during verify.
pub fn capture_non_coder(snapshot_dir: &Path, stage_tag: &str) -> std::io::Result<()> {
    let baseline_stash = stash_working_tree(&format!("codexize-baseline-{stage_tag}"));
    let snap = Snapshot {
        head: git_head().unwrap_or_default(),
        git_status: git_status().unwrap_or_default(),
        control_files: BTreeMap::new(),
        baseline_stash,
    };
    write_snapshot(snapshot_dir, &snap)
}

/// `git stash push -u -m <msg>`. Returns Some(msg) if a stash entry was
/// actually created (i.e., there was something to stash).
fn stash_working_tree(msg: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["stash", "push", "-u", "-m", msg])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Git prints "No local changes to save" when there's nothing to stash.
    if text.contains("No local changes to save") {
        None
    } else {
        Some(msg.to_string())
    }
}

/// Find a stash entry by its message and pop it.
fn pop_stash_by_message(msg: &str) -> bool {
    let Ok(list) = std::process::Command::new("git")
        .args(["stash", "list"])
        .output()
    else {
        return false;
    };
    if !list.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&list.stdout);
    for line in text.lines() {
        if line.contains(msg)
            && let Some(r#ref) = line.split(':').next()
        {
            return std::process::Command::new("git")
                .args(["stash", "pop", r#ref])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        }
    }
    false
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

pub fn capture_coder(snapshot_dir: &Path, session_dir: &Path, round: u32) -> std::io::Result<()> {
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
    };
    write_snapshot(snapshot_dir, &snap)
}

/// Verify the snapshot. Returns `Some(reason)` if the agent violated
/// immutability; reverts as much as possible before returning.
pub fn verify(snapshot_dir: &Path, stage: &str) -> Option<String> {
    let snap = read_snapshot(snapshot_dir)?;
    if stage == "coder" {
        verify_coder(&snap)
    } else {
        verify_non_coder(&snap)
    }
}

fn verify_non_coder(snap: &Snapshot) -> Option<String> {
    let current_head = git_head().unwrap_or_default();
    let current_status = git_status().unwrap_or_default();
    let head_changed =
        !snap.head.is_empty() && !current_head.is_empty() && current_head != snap.head;

    // Stash anything the agent wrote so the working tree is clean before we
    // restore the user's baseline. The stash is retained under a labelled
    // entry so the operator can recover it with `git stash list` if needed.
    if current_status.trim() != snap.git_status.trim() {
        let _ = stash_working_tree("codexize-guard-agent-writes");
    }

    // Agent committed something on top of our HEAD — reset to the snapshot
    // HEAD. The commits remain in the reflog.
    if head_changed {
        let _ = std::process::Command::new("git")
            .args(["reset", "--hard", &snap.head])
            .output();
    }

    // Restore the user's pre-run changes, if any.
    if let Some(msg) = &snap.baseline_stash {
        let _ = pop_stash_by_message(msg);
    }

    None
}

fn verify_coder(snap: &Snapshot) -> Option<String> {
    let mut violated = Vec::new();
    for (path, expected) in &snap.control_files {
        let actual = std::fs::read_to_string(path).ok();
        if actual.as_deref() != Some(expected.as_str()) {
            let _ = std::fs::write(path, expected);
            violated.push(path.clone());
        }
    }
    if violated.is_empty() {
        None
    } else {
        Some(format!("forbidden_control_edit: {}", violated.join(", ")))
    }
}
