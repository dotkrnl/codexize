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

pub fn git_status_dirty() -> bool {
    git_status()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
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

/// Capture a snapshot for a non-coder agent. Records HEAD and working-tree
/// status so `verify_non_coder` can detect changes and emit warnings.
pub fn capture_non_coder(snapshot_dir: &Path, _stage_tag: &str) -> std::io::Result<()> {
    let snap = Snapshot {
        head: git_head().unwrap_or_default(),
        git_status: git_status().unwrap_or_default(),
        control_files: BTreeMap::new(),
        baseline_stash: None,
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

/// Verify the snapshot. Returns `(hard_error, warnings)` where the first
/// element is a protocol-violation reason (only HEAD-advance for non-coder)
/// and the second is advisory warnings for the operator.
pub fn verify(snapshot_dir: &Path, stage: &str) -> (Option<String>, Vec<String>) {
    let Some(snap) = read_snapshot(snapshot_dir) else {
        return (None, vec![]);
    };
    if stage == "coder" {
        (verify_coder(&snap), vec![])
    } else {
        verify_non_coder(&snap)
    }
}

fn verify_non_coder(snap: &Snapshot) -> (Option<String>, Vec<String>) {
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
        let _ = std::process::Command::new("git")
            .args(["reset", "--hard", &snap.head])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        return (Some("forbidden_head_advance".to_string()), warnings);
    }

    (None, warnings)
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
