//! Staged installer for vendor brainstorming packages.
//!
//! For each target, the installer renders the vendor adapter into a sibling
//! staging directory, validates required files, then atomically swaps the
//! staged tree into place. Replacement is destructive — the spec rejects
//! local-edit preservation — but no backups remain on disk after the swap.
//!
//! Per-vendor failures are isolated. The caller (sync orchestrator) feeds
//! successful outcomes into [`super::metadata::BrainstormMetadata`] so
//! metadata advances only for vendors actually replaced; failed vendors keep
//! their previous record. Spec: install plan §10, error handling §"One
//! vendor install fails".
//!
//! Source-of-truth precondition: the upstream root must contain
//! `skills/brainstorming/`. If it does not, [`install_packages`] aborts the
//! entire batch before touching any target — there is nothing safe to copy.

use super::adapter::{SKILL_FILE, render_package};
use super::metadata::InstallMode;
use crate::selection::VendorKind;
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

/// One vendor's install request: where to put the rendered package and which
/// vendor preamble to apply. The installer does not consult metadata or
/// discovery — it only acts on what the planner already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    pub vendor: VendorKind,
    pub mode: InstallMode,
    pub path: PathBuf,
}

/// Result of attempting to install one vendor's package. `error` is `None`
/// on success; otherwise the message can be surfaced through preflight
/// status without aborting the rest of the batch.
#[derive(Debug)]
pub struct InstallOutcome {
    pub vendor: VendorKind,
    pub mode: InstallMode,
    pub path: PathBuf,
    pub commit: String,
    pub error: Option<anyhow::Error>,
}

impl InstallOutcome {
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Install every target from a previously fetched/cached source rooted at
/// `upstream_root` (the directory whose `skills/brainstorming/` subtree is
/// the upstream package). `commit` is recorded on each successful outcome so
/// the orchestrator can advance per-vendor metadata.
///
/// Returns `Err` only when the upstream root itself is unusable (missing
/// `skills/brainstorming/`). Per-vendor failures land inside the returned
/// `Vec` so partial success keeps moving forward — the spec requires
/// vendor-level isolation.
pub fn install_packages(
    upstream_root: &Path,
    commit: &str,
    targets: &[InstallTarget],
) -> Result<Vec<InstallOutcome>> {
    let upstream_pkg = validate_upstream_root(upstream_root)?;
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in targets {
        let result = install_one(&upstream_pkg, target);
        outcomes.push(InstallOutcome {
            vendor: target.vendor,
            mode: target.mode,
            path: target.path.clone(),
            commit: commit.to_string(),
            error: result.err(),
        });
    }
    Ok(outcomes)
}

/// Returns the upstream `skills/brainstorming/` directory, or an error
/// suitable for surfacing to the caller. Centralized so the planner and
/// installer reject the same precondition the same way.
pub fn validate_upstream_root(upstream_root: &Path) -> Result<PathBuf> {
    let pkg = upstream_root.join("skills").join("brainstorming");
    if !pkg.is_dir() {
        return Err(anyhow!(
            "upstream source at {} is missing skills/brainstorming/",
            upstream_root.display()
        ));
    }
    Ok(pkg)
}

fn install_one(upstream_pkg: &Path, target: &InstallTarget) -> Result<()> {
    let parent = target.path.parent().ok_or_else(|| {
        anyhow!(
            "install target has no parent directory: {}",
            target.path.display()
        )
    })?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create install parent directory {}",
            parent.display()
        )
    })?;

    let staging = unique_sibling(&target.path, "staging");
    // Render writes into `staging`; render_package itself creates the dir.
    let render_result = render_package(upstream_pkg, target.vendor, &staging);
    let render_outcome = match render_result {
        Ok(()) => validate_staged(&staging),
        Err(e) => Err(e),
    };
    if let Err(err) = render_outcome {
        // Best-effort: don't leave a half-rendered staging dir behind.
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }

    swap_into_place(&staging, &target.path).with_context(|| {
        format!(
            "failed to install brainstorming package at {}",
            target.path.display()
        )
    })?;
    Ok(())
}

/// Validate that the staged package contains the files the installer
/// promises. Today that's just `SKILL.md`; centralizing the check makes it
/// easy to extend without sprinkling new asserts across the installer.
fn validate_staged(staging: &Path) -> Result<()> {
    let skill = staging.join(SKILL_FILE);
    if !skill.is_file() {
        return Err(anyhow!(
            "staged package missing required file {}",
            skill.display()
        ));
    }
    Ok(())
}

/// Replace `target` with the staged directory `staging`. Leaves no backup
/// behind on success and restores the previous target on failure when
/// possible.
///
/// POSIX `rename(2)` of a directory over an existing directory is not
/// portable, so we do the swap in two renames: first move the old target
/// to a unique sibling, then move the staged dir into place, then remove
/// the old. That keeps the swap point atomic from the caller's viewpoint —
/// either the old dir is in place, or the new dir is.
fn swap_into_place(staging: &Path, target: &Path) -> Result<()> {
    swap_into_place_with(staging, target, |path| std::fs::remove_dir_all(path))
}

fn swap_into_place_with(
    staging: &Path,
    target: &Path,
    remove_backup: fn(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let old_holder = if target.exists() {
        let bak = unique_sibling(target, "previous");
        std::fs::rename(target, &bak).with_context(|| {
            format!(
                "failed to move existing {} to {}",
                target.display(),
                bak.display()
            )
        })?;
        Some(bak)
    } else {
        None
    };

    if let Err(err) = std::fs::rename(staging, target) {
        // Restore the original so the vendor isn't left empty.
        if let Some(ref bak) = old_holder {
            let _ = std::fs::rename(bak, target);
        }
        // Drop staged work; we did not promote it.
        let _ = std::fs::remove_dir_all(staging);
        return Err(anyhow::Error::from(err).context(format!(
            "failed to move staged package into {}",
            target.display()
        )));
    }

    if let Some(bak) = old_holder {
        // Ambiguous edge case note: when post-promotion cleanup fails we
        // fail the install and restore the previous target, because this task
        // requires successful installs to leave no backup behind.
        if let Err(cleanup_err) = remove_backup(&bak) {
            let rollback = || -> Result<()> {
                std::fs::remove_dir_all(target).with_context(|| {
                    format!(
                        "cleanup rollback could not remove promoted target {}",
                        target.display()
                    )
                })?;
                std::fs::rename(&bak, target).with_context(|| {
                    format!(
                        "cleanup rollback could not restore previous target {}",
                        target.display()
                    )
                })?;
                Ok(())
            };
            match rollback() {
                Ok(()) => {
                    return Err(anyhow::Error::new(cleanup_err)
                        .context(format!("failed backup cleanup for {}", bak.display())));
                }
                Err(rollback_err) => {
                    return Err(rollback_err
                        .context(format!("failed backup cleanup for {}", bak.display())));
                }
            }
        }
    }
    Ok(())
}

/// Build a sibling path under `target`'s parent that is unique to this
/// process and call site. Avoids pulling in `uuid` for what is effectively
/// a per-install scratch name.
fn unique_sibling(target: &Path, role: &str) -> PathBuf {
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("brainstorming");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = next_counter();
    parent.join(format!(".{stem}.{role}.{pid}.{nanos}.{counter}"))
}

fn next_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_upstream_root(brainstorming: bool, body: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        if brainstorming {
            let pkg = dir.path().join("skills").join("brainstorming");
            std::fs::create_dir_all(&pkg).unwrap();
            std::fs::write(pkg.join(SKILL_FILE), body).unwrap();
            std::fs::write(pkg.join("notes.md"), "support\n").unwrap();
        } else {
            std::fs::write(dir.path().join("README.md"), "no skills\n").unwrap();
        }
        dir
    }

    fn target_under(dir: &Path, vendor: VendorKind, mode: InstallMode) -> InstallTarget {
        let path = dir
            .join("vendor")
            .join(vendor_dir(vendor))
            .join("brainstorming");
        InstallTarget { vendor, mode, path }
    }

    fn vendor_dir(vendor: VendorKind) -> &'static str {
        match vendor {
            VendorKind::Codex => "codex",
            VendorKind::Claude => "claude",
            VendorKind::Gemini => "gemini",
            VendorKind::Kimi => "kimi",
        }
    }

    fn list_parent_entries(target: &Path) -> Vec<String> {
        let parent = target.parent().unwrap();
        std::fs::read_dir(parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn install_creates_target_when_absent() {
        let upstream = make_upstream_root(true, "# upstream body\n");
        let work = TempDir::new().unwrap();
        let target = target_under(work.path(), VendorKind::Codex, InstallMode::Fallback);

        let outcomes =
            install_packages(upstream.path(), "abc1234", std::slice::from_ref(&target)).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].is_success(), "{:?}", outcomes[0].error);
        assert_eq!(outcomes[0].commit, "abc1234");
        assert!(target.path.join(SKILL_FILE).is_file());
        assert!(target.path.join("notes.md").is_file());
        let skill = std::fs::read_to_string(target.path.join(SKILL_FILE)).unwrap();
        assert!(skill.contains("Codex adapter"));
        assert!(skill.contains("upstream body"));
    }

    #[test]
    fn install_replaces_existing_target_destructively() {
        let upstream = make_upstream_root(true, "# new body\n");
        let work = TempDir::new().unwrap();
        let target = target_under(work.path(), VendorKind::Claude, InstallMode::Native);

        // Pre-populate target with stale content to prove the installer
        // overwrites it without keeping any of it.
        std::fs::create_dir_all(&target.path).unwrap();
        std::fs::write(target.path.join("STALE.md"), "remove me").unwrap();
        std::fs::write(target.path.join(SKILL_FILE), "old skill").unwrap();

        let outcomes =
            install_packages(upstream.path(), "fffffff", std::slice::from_ref(&target)).unwrap();
        assert!(outcomes[0].is_success(), "{:?}", outcomes[0].error);
        assert!(
            !target.path.join("STALE.md").exists(),
            "stale file survived"
        );
        let skill = std::fs::read_to_string(target.path.join(SKILL_FILE)).unwrap();
        assert!(skill.contains("Claude adapter"));
        assert!(skill.contains("new body"));
    }

    #[test]
    fn install_leaves_no_backup_or_staging_dirs_on_success() {
        let upstream = make_upstream_root(true, "# body\n");
        let work = TempDir::new().unwrap();
        let target = target_under(work.path(), VendorKind::Gemini, InstallMode::Fallback);
        std::fs::create_dir_all(&target.path).unwrap();
        std::fs::write(target.path.join("legacy.md"), "x").unwrap();

        install_packages(upstream.path(), "deadbee", std::slice::from_ref(&target)).unwrap();

        let entries = list_parent_entries(&target.path);
        // Only the target directory should remain — no .previous/.staging
        // siblings should leak through.
        assert_eq!(entries, vec!["brainstorming".to_string()]);
    }

    #[test]
    fn install_aborts_when_upstream_missing_brainstorming_dir() {
        let upstream = make_upstream_root(false, "ignored");
        let work = TempDir::new().unwrap();
        let target = target_under(work.path(), VendorKind::Codex, InstallMode::Fallback);
        let err = install_packages(upstream.path(), "abc", std::slice::from_ref(&target))
            .expect_err("missing upstream brainstorming/ should abort");
        assert!(err.to_string().contains("skills/brainstorming"), "{err}");
        assert!(!target.path.exists(), "target must remain unchanged");
    }

    #[test]
    fn install_isolates_per_vendor_failure() {
        let upstream = make_upstream_root(true, "# body\n");
        let work = TempDir::new().unwrap();
        let good = target_under(work.path(), VendorKind::Codex, InstallMode::Fallback);
        // Force failure for the second target by making the planned parent
        // path collide with an existing regular file. install_one will try
        // create_dir_all on `<work>/vendor/claude` — a file there blocks it.
        let claude_root = work.path().join("vendor").join("claude");
        std::fs::create_dir_all(work.path().join("vendor")).unwrap();
        std::fs::write(&claude_root, "not a directory").unwrap();
        let bad = InstallTarget {
            vendor: VendorKind::Claude,
            mode: InstallMode::Fallback,
            path: claude_root.join("brainstorming"),
        };

        let outcomes =
            install_packages(upstream.path(), "abc1234", &[good.clone(), bad.clone()]).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].is_success(), "{:?}", outcomes[0].error);
        assert!(outcomes[1].error.is_some(), "claude should fail");
        // The successful vendor was still installed.
        assert!(good.path.join(SKILL_FILE).is_file());
    }

    #[test]
    fn install_uses_cached_source_layout() {
        // Same shape an offline install would reach for: a previously cached
        // upstream tree on disk, no network.
        let cache = TempDir::new().unwrap();
        let pkg = cache.path().join("skills").join("brainstorming");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join(SKILL_FILE), "# cached upstream\n").unwrap();
        std::fs::write(pkg.join("references.md"), "ref\n").unwrap();

        let work = TempDir::new().unwrap();
        let target = target_under(work.path(), VendorKind::Kimi, InstallMode::Fallback);
        let outcomes =
            install_packages(cache.path(), "cached1", std::slice::from_ref(&target)).unwrap();
        assert!(outcomes[0].is_success());
        let skill = std::fs::read_to_string(target.path.join(SKILL_FILE)).unwrap();
        assert!(skill.contains("Kimi adapter"));
        assert!(skill.contains("cached upstream"));
        assert!(target.path.join("references.md").is_file());
    }

    #[test]
    fn metadata_only_advances_for_successful_outcomes() {
        // The installer doesn't write metadata itself, but the orchestrator
        // pattern is: iterate outcomes and only advance metadata for
        // successes. Verify that pattern works with the produced outcomes.
        use crate::brainstorm_sync::metadata::{BrainstormMetadata, VendorRecord};

        let upstream = make_upstream_root(true, "# body\n");
        let work = TempDir::new().unwrap();
        let good = target_under(work.path(), VendorKind::Codex, InstallMode::Fallback);
        let bad_root = work.path().join("vendor").join("claude");
        std::fs::create_dir_all(work.path().join("vendor")).unwrap();
        std::fs::write(&bad_root, "blocker").unwrap();
        let bad = InstallTarget {
            vendor: VendorKind::Claude,
            mode: InstallMode::Fallback,
            path: bad_root.join("brainstorming"),
        };

        let mut metadata = BrainstormMetadata::default();
        // Pre-existing record for Claude must survive the failure.
        metadata.set_vendor_record(
            VendorKind::Claude,
            VendorRecord {
                installed_commit: "previous".into(),
                path: bad.path.clone(),
                mode: InstallMode::Fallback,
            },
        );

        let outcomes =
            install_packages(upstream.path(), "freshcommit", &[good.clone(), bad.clone()]).unwrap();
        for outcome in &outcomes {
            if outcome.is_success() {
                metadata.set_vendor_record(
                    outcome.vendor,
                    VendorRecord {
                        installed_commit: outcome.commit.clone(),
                        path: outcome.path.clone(),
                        mode: outcome.mode,
                    },
                );
            }
        }

        let codex = metadata
            .vendor_record(VendorKind::Codex)
            .expect("codex installed");
        assert_eq!(codex.installed_commit, "freshcommit");
        let claude = metadata
            .vendor_record(VendorKind::Claude)
            .expect("claude record kept");
        assert_eq!(
            claude.installed_commit, "previous",
            "failed vendor must keep prior commit"
        );
    }

    #[test]
    fn validate_upstream_root_accepts_directory_with_brainstorming() {
        let dir = TempDir::new().unwrap();
        let pkg = dir.path().join("skills").join("brainstorming");
        std::fs::create_dir_all(&pkg).unwrap();
        let resolved = validate_upstream_root(dir.path()).unwrap();
        assert_eq!(resolved, pkg);
    }

    #[test]
    fn validate_upstream_root_rejects_missing_brainstorming() {
        let dir = TempDir::new().unwrap();
        let err = validate_upstream_root(dir.path()).unwrap_err();
        assert!(err.to_string().contains("skills/brainstorming"));
    }

    #[test]
    fn install_handles_empty_target_list() {
        let upstream = make_upstream_root(true, "# body\n");
        let outcomes = install_packages(upstream.path(), "x", &[]).unwrap();
        assert!(outcomes.is_empty());
    }

    #[test]
    fn swap_fails_when_backup_cleanup_fails_and_restores_previous_target() {
        let dir = TempDir::new().unwrap();
        let parent = dir.path().join("vendor").join("codex");
        std::fs::create_dir_all(&parent).unwrap();
        let target = parent.join("brainstorming");
        let staging = parent.join(".brainstorming.staging.test");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(target.join(SKILL_FILE), "old").unwrap();
        std::fs::write(staging.join(SKILL_FILE), "new").unwrap();

        let err = swap_into_place_with(&staging, &target, |_bak| {
            Err(std::io::Error::other("simulated cleanup failure"))
        })
        .expect_err("cleanup failure should fail install");
        assert!(
            err.to_string().contains("cleanup"),
            "unexpected error: {err}"
        );
        let current = std::fs::read_to_string(target.join(SKILL_FILE)).unwrap();
        assert_eq!(current, "old");
        assert!(!staging.exists(), "staging should not remain after failure");
    }
}
