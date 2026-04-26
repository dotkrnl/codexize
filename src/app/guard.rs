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

/// Decided at run-launch time and persisted on the snapshot so finalization
/// and resume paths cannot lose the choice. `AutoReset` keeps today's
/// behavior (fail closed; `git reset --hard` on HEAD-advance). `AskOperator`
/// is reserved for interactive non-coder runs: verify reports a pending
/// decision instead of resetting, and the operator chooses reset vs keep.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GuardMode {
    #[default]
    AutoReset,
    AskOperator,
}

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
    /// How the guard should react to a HEAD-advance violation. Defaulted so
    /// snapshot files written before this field existed deserialize as
    /// `AutoReset`.
    #[serde(default)]
    pub mode: GuardMode,
    /// Reviewer-only working-tree baseline when dirty changes are in scope.
    #[serde(default)]
    pub working_tree_baseline: Option<String>,
}

/// Outcome of verifying a snapshot. Three arms so callers cannot
/// accidentally treat a pending operator decision as a hard error.
#[derive(Debug, Clone)]
pub enum VerifyResult {
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
    git_status().map(|s| !s.trim().is_empty()).unwrap_or(false)
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

fn git_diff_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
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
pub fn capture_non_coder(
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
        mode: GuardMode::default(),
        working_tree_baseline: None,
    };
    write_snapshot(snapshot_dir, &snap)
}

/// Verify the snapshot. Returns a typed three-arm result so callers cannot
/// accidentally treat a pending operator decision as a hard error.
pub fn verify(snapshot_dir: &Path, stage: &str) -> VerifyResult {
    let Some(snap) = read_snapshot(snapshot_dir) else {
        return VerifyResult::Ok { warnings: vec![] };
    };
    if stage == "coder" {
        verify_coder(&snap)
    } else {
        verify_non_coder(&snap)
    }
}

/// Run `git reset --hard <captured_head>` so an operator-driven reset can
/// share the exact reset path used by `AutoReset` mode without having to
/// re-read the snapshot.
pub fn reset_hard_to(captured_head: &str) -> bool {
    if captured_head.is_empty() {
        return false;
    }
    std::process::Command::new("git")
        .args(["reset", "--hard", captured_head])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
        match snap.mode {
            GuardMode::AutoReset => {
                let _ = std::process::Command::new("git")
                    .args(["reset", "--hard", &snap.head])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output();
                return VerifyResult::HardError {
                    reason: "forbidden_head_advance".to_string(),
                    warnings,
                };
            }
            GuardMode::AskOperator => {
                return VerifyResult::PendingDecision {
                    captured_head: snap.head.clone(),
                    current_head,
                    warnings,
                };
            }
        }
    }

    if let Some(expected) = &snap.working_tree_baseline {
        let current = git_working_tree_baseline().unwrap_or_default();
        if &current != expected {
            return VerifyResult::HardError {
                reason: "reviewer_modified_working_tree".to_string(),
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
            reason: format!("forbidden_control_edit: {}", violated.join(", ")),
            warnings: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warnings_of(result: &VerifyResult) -> Vec<String> {
        match result {
            VerifyResult::Ok { warnings }
            | VerifyResult::HardError { warnings, .. }
            | VerifyResult::PendingDecision { warnings, .. } => warnings.clone(),
        }
    }

    #[test]
    fn verify_non_coder_warns_on_pre_dirty_status() {
        let head = git_head().unwrap_or_default();
        let current_status = git_status().unwrap_or_default();
        let snap = test_snapshot(&head, &format!("{current_status} M dirty.txt\n"));
        let result = verify_non_coder(&snap);
        assert!(matches!(result, VerifyResult::Ok { .. }));
        let warnings = warnings_of(&result);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("dirty before agent launch")),
            "expected dirty-tree warning, got: {warnings:?}"
        );
    }

    #[test]
    fn verify_non_coder_warns_on_changed_status() {
        let head = git_head().unwrap_or_default();
        let current_status = git_status().unwrap_or_default();
        let snap = test_snapshot(&head, &format!("{current_status}?? phantom-file.xyz\n"));
        let result = verify_non_coder(&snap);
        assert!(matches!(result, VerifyResult::Ok { .. }));
        let warnings = warnings_of(&result);
        assert!(
            warnings.iter().any(|w| w.contains("modified working tree")),
            "expected modified-tree warning, got: {warnings:?}"
        );
    }

    #[test]
    fn verify_non_coder_hard_error_on_head_advance_auto_reset() {
        let snap = test_snapshot("0000000000000000000000000000000000000000", "");
        let result = verify_non_coder(&snap);
        match result {
            VerifyResult::HardError { reason, .. } => {
                assert_eq!(reason, "forbidden_head_advance");
            }
            other => panic!("expected HardError, got {other:?}"),
        }
    }

    #[test]
    fn verify_non_coder_pending_on_head_advance_ask_operator() {
        let mut snap = test_snapshot("0000000000000000000000000000000000000000", "");
        snap.mode = GuardMode::AskOperator;
        let current = git_head().unwrap_or_default();
        let result = verify_non_coder(&snap);
        match result {
            VerifyResult::PendingDecision {
                captured_head,
                current_head,
                ..
            } => {
                assert_eq!(captured_head, "0000000000000000000000000000000000000000");
                assert_eq!(current_head, current);
                // Confirm we did NOT reset: HEAD must still match what we
                // observed before calling verify. (verify already read it
                // once; if reset had happened, current_head would equal
                // captured_head — which we explicitly assert is not the
                // captured zero-SHA above.)
                assert_ne!(current_head, captured_head);
            }
            other => panic!("expected PendingDecision, got {other:?}"),
        }
    }

    #[test]
    fn verify_non_coder_matching_status_has_no_modified_warning() {
        let head = git_head().unwrap_or_default();
        let status = git_status().unwrap_or_default();
        let snap = test_snapshot(&head, &status);
        let result = verify_non_coder(&snap);
        assert!(matches!(result, VerifyResult::Ok { .. }));
        let warnings = warnings_of(&result);
        assert!(
            !warnings.iter().any(|w| w.contains("modified working tree")),
            "expected no modified-tree warning when status unchanged, got: {warnings:?}"
        );
    }

    #[test]
    fn verify_non_coder_hard_error_when_dirty_baseline_changes() {
        let head = git_head().unwrap_or_default();
        let status = git_status().unwrap_or_default();
        let mut snap = test_snapshot(&head, &status);
        snap.working_tree_baseline = Some("__baseline_that_should_not_match__".to_string());

        let result = verify_non_coder(&snap);
        match result {
            VerifyResult::HardError { reason, .. } => {
                assert_eq!(reason, "reviewer_modified_working_tree");
            }
            other => panic!("expected HardError, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_deserializes_without_mode_as_auto_reset() {
        // Pre-existing on-disk snapshots predate the `mode` field and must
        // load as AutoReset to preserve today's behavior on resume.
        let toml_text = r#"
head = "abc123"
git_status = ""
"#;
        let snap: Snapshot = toml::from_str(toml_text).expect("deserialize legacy snapshot");
        assert_eq!(snap.mode, GuardMode::AutoReset);
        assert_eq!(snap.head, "abc123");
    }

    #[test]
    fn guard_mode_round_trips_through_toml() {
        let snap = Snapshot {
            head: "deadbeef".to_string(),
            git_status: String::new(),
            control_files: BTreeMap::new(),
            baseline_stash: None,
            mode: GuardMode::AskOperator,
            working_tree_baseline: None,
        };
        let text = toml::to_string(&snap).expect("serialize");
        assert!(
            text.contains("ask_operator"),
            "expected snake_case variant in: {text}"
        );
        let back: Snapshot = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.mode, GuardMode::AskOperator);
    }
}
