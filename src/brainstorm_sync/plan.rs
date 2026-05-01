//! Pure planning for the brainstorm sync subsystem.
//!
//! Responsibilities:
//! * 24-hour freshness gate over `last_checked_at`, with future and
//!   unparseable timestamps treated as expired.
//! * Missing-package detection from the filesystem state of each target.
//! * Commit-difference staleness using the latest known upstream commit.
//! * Batch plan construction so preflight UI and the installer share one
//!   structured view of what needs to change.
//!
//! This module never performs I/O against the upstream and never replaces
//! files. It consumes already-discovered targets, the local metadata, the
//! current time, and (optionally) a freshly resolved upstream commit.

use super::discovery::VendorTarget;
use super::metadata::{BrainstormMetadata, InstallMode};
use crate::selection::VendorKind;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Why a vendor's brainstorming package needs to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallReason {
    /// The target package directory does not exist on disk.
    Missing,
    /// The recorded installed commit differs from `latest`. The spec
    /// requires *difference*, not "older than" semantics, so a forced
    /// downgrade is also stale.
    StaleCommit { installed: String, latest: String },
    /// The package exists but no commit has ever been recorded — treat as
    /// stale so a confirmed install brings it under management.
    UnknownInstalledCommit { latest: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorPlan {
    pub vendor: VendorKind,
    pub mode: InstallMode,
    pub path: PathBuf,
    pub installed_commit: Option<String>,
    pub reason: InstallReason,
}

/// Whether the planner can act on the result automatically. Non-interactive
/// startup never gets to display a confirmation modal, so the installer
/// must skip in that case even if work is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOfferability {
    Interactive,
    NonInteractive,
}

/// What to do about the upstream after evaluating local state. The planner
/// is purely a recommendation; the orchestrator decides whether to honor
/// it (e.g. it might skip remote calls when offline). Source state is
/// computed independently from `latest_known_upstream_commit` and any
/// cached source so a missing local package can still be planned offline
/// when cached source is sufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamCheck {
    /// Inside the 24-hour window with all local packages present —
    /// no remote check needed this run.
    Skip,
    /// The 24-hour window has expired (or `last_checked_at` is bad/future);
    /// fetch the latest commit when possible, but do not block startup if
    /// it fails.
    Refresh,
    /// At least one eligible vendor is missing locally and cached source
    /// either does not exist or does not match the latest known commit.
    /// Refresh and acquire source before the modal can offer an install.
    RefreshForMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    pub upstream_url: String,
    pub latest_known_commit: Option<String>,
    pub vendors: Vec<VendorPlan>,
    pub upstream_check: UpstreamCheck,
    pub offerability: PlanOfferability,
}

impl BatchPlan {
    pub fn is_empty(&self) -> bool {
        self.vendors.is_empty()
    }

    pub fn vendors_by_mode(&self, mode: InstallMode) -> Vec<&VendorPlan> {
        self.vendors.iter().filter(|p| p.mode == mode).collect()
    }
}

/// 24-hour freshness window. Pulled out as a constant so tests can read
/// the boundary in human terms.
pub const FRESHNESS_WINDOW: Duration = Duration::hours(24);

/// Decide whether the upstream commit lookup is required for this run.
///
/// Spec rules:
/// * Refresh when `now - last_checked_at >= 24h`.
/// * Refresh-for-missing when any eligible package is locally missing AND
///   the cached source cannot satisfy that install.
/// * Skip when inside the 24-hour window and every eligible package is
///   present locally.
/// * Future/malformed timestamps are treated as expired.
pub fn evaluate_upstream_check(
    now: DateTime<Utc>,
    metadata: &BrainstormMetadata,
    targets: &BTreeMap<VendorKind, VendorTarget>,
) -> UpstreamCheck {
    let any_missing = targets.values().any(|t| !package_exists(&t.path));

    let cached_source_satisfies_missing = match (
        metadata.cached_source.as_ref(),
        metadata.latest_known_upstream_commit.as_deref(),
    ) {
        (Some(src), Some(latest)) => src.commit == latest && Path::new(&src.path).exists(),
        _ => false,
    };

    if any_missing && !cached_source_satisfies_missing {
        return UpstreamCheck::RefreshForMissing;
    }

    let last = match metadata.parsed_last_checked_at(now) {
        Some(ts) => ts,
        // Future/malformed timestamp or never-checked: refresh.
        None => return UpstreamCheck::Refresh,
    };
    if now.signed_duration_since(last) >= FRESHNESS_WINDOW {
        UpstreamCheck::Refresh
    } else {
        UpstreamCheck::Skip
    }
}

/// Build the batch plan given local state and (optionally) a freshly
/// resolved upstream commit. `latest_remote_commit` should be passed when
/// a successful upstream check has produced a commit this run; otherwise
/// the planner falls back to `metadata.latest_known_upstream_commit`.
pub fn build_plan(
    now: DateTime<Utc>,
    metadata: &BrainstormMetadata,
    targets: &BTreeMap<VendorKind, VendorTarget>,
    latest_remote_commit: Option<&str>,
    upstream_url: String,
    offerability: PlanOfferability,
) -> BatchPlan {
    let upstream_check = evaluate_upstream_check(now, metadata, targets);
    let latest = latest_remote_commit
        .map(str::to_string)
        .or_else(|| metadata.latest_known_upstream_commit.clone());

    let mut vendors = Vec::new();
    for (vendor, target) in targets {
        let exists = package_exists(&target.path);
        let recorded_commit = metadata
            .vendor_record(*vendor)
            .map(|r| r.installed_commit.clone());

        let reason = if !exists {
            Some(InstallReason::Missing)
        } else {
            match (recorded_commit.as_deref(), latest.as_deref()) {
                (Some(installed), Some(latest_commit)) if installed != latest_commit => {
                    Some(InstallReason::StaleCommit {
                        installed: installed.to_string(),
                        latest: latest_commit.to_string(),
                    })
                }
                (None, Some(latest_commit)) => Some(InstallReason::UnknownInstalledCommit {
                    latest: latest_commit.to_string(),
                }),
                _ => None,
            }
        };

        if let Some(reason) = reason {
            vendors.push(VendorPlan {
                vendor: *vendor,
                mode: target.mode,
                path: target.path.clone(),
                installed_commit: recorded_commit,
                reason,
            });
        }
    }

    BatchPlan {
        upstream_url,
        latest_known_commit: latest,
        vendors,
        upstream_check,
        offerability,
    }
}

/// A package is considered installed when its target directory exists.
/// Deeper validation (presence of `SKILL.md`, etc.) is the installer's
/// concern; here we only need a fast presence check that matches what
/// users see on disk.
fn package_exists(path: &Path) -> bool {
    path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brainstorm_sync::discovery::VendorTarget;
    use crate::brainstorm_sync::metadata::{CachedSource, VendorRecord};
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 30, 12, 0, 0).unwrap()
    }

    fn make_target(vendor: VendorKind, path: PathBuf, mode: InstallMode) -> VendorTarget {
        VendorTarget { vendor, mode, path }
    }

    fn create_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
    }

    fn fresh_within_window(now: DateTime<Utc>) -> String {
        // 1 hour ago
        (now - Duration::hours(1)).to_rfc3339()
    }

    fn just_outside_window(now: DateTime<Utc>) -> String {
        // 25 hours ago
        (now - Duration::hours(25)).to_rfc3339()
    }

    #[test]
    fn skips_remote_when_inside_window_and_packages_present() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("codex");
        create_dir(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, path, InstallMode::Native),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            ..Default::default()
        };
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::Skip
        );
    }

    #[test]
    fn refreshes_when_outside_24h_window() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("codex");
        create_dir(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some(just_outside_window(now())),
            ..Default::default()
        };
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::Refresh
        );
    }

    #[test]
    fn future_timestamp_is_expired() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("codex");
        create_dir(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some("2099-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::Refresh
        );
    }

    #[test]
    fn unparseable_timestamp_is_expired() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("codex");
        create_dir(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some("not-a-date".into()),
            ..Default::default()
        };
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::Refresh
        );
    }

    #[test]
    fn missing_package_inside_window_forces_refresh_for_missing() {
        let dir = TempDir::new().unwrap();
        // Note: target path intentionally does NOT exist on disk.
        let missing_path = dir.path().join("never-created");
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, missing_path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            ..Default::default()
        };
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::RefreshForMissing
        );
    }

    #[test]
    fn missing_package_with_matching_cached_source_does_not_force_refresh() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("cache");
        create_dir(&cache);
        let missing_path = dir.path().join("never-created");
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, missing_path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            latest_known_upstream_commit: Some("aaa".into()),
            cached_source: Some(CachedSource {
                commit: "aaa".into(),
                path: cache,
            }),
            ..Default::default()
        };
        // Cached source matches latest known commit and exists on disk:
        // the planner can install offline, so no remote refresh is needed
        // *for the missing package*. Inside the freshness window we Skip.
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::Skip
        );
    }

    #[test]
    fn missing_package_with_stale_cached_source_still_forces_refresh() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("cache");
        create_dir(&cache);
        let missing_path = dir.path().join("never-created");
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, missing_path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            latest_known_upstream_commit: Some("bbb".into()),
            cached_source: Some(CachedSource {
                commit: "aaa".into(),
                path: cache,
            }),
            ..Default::default()
        };
        assert_eq!(
            evaluate_upstream_check(now(), &metadata, &targets),
            UpstreamCheck::RefreshForMissing
        );
    }

    #[test]
    fn build_plan_marks_missing_packages_as_install_targets() {
        let dir = TempDir::new().unwrap();
        let codex_path = dir.path().join("never-codex");
        let claude_path = dir.path().join("claude");
        create_dir(&claude_path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, codex_path.clone(), InstallMode::Fallback),
        );
        targets.insert(
            VendorKind::Claude,
            make_target(
                VendorKind::Claude,
                claude_path.clone(),
                InstallMode::Fallback,
            ),
        );
        let mut metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            latest_known_upstream_commit: Some("aaa".into()),
            ..Default::default()
        };
        // Claude is recorded as already up to date, so it should not appear
        // in the plan.
        metadata.set_vendor_record(
            VendorKind::Claude,
            VendorRecord {
                installed_commit: "aaa".into(),
                path: claude_path,
                mode: InstallMode::Fallback,
            },
        );

        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            None,
            "https://example.test".into(),
            PlanOfferability::Interactive,
        );

        assert_eq!(plan.vendors.len(), 1);
        let only = &plan.vendors[0];
        assert_eq!(only.vendor, VendorKind::Codex);
        assert_eq!(only.path, codex_path);
        assert_eq!(only.reason, InstallReason::Missing);
        assert_eq!(plan.latest_known_commit.as_deref(), Some("aaa"));
    }

    #[test]
    fn build_plan_detects_commit_difference_staleness() {
        let dir = TempDir::new().unwrap();
        let codex_path = dir.path().join("codex");
        create_dir(&codex_path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, codex_path.clone(), InstallMode::Native),
        );
        let mut metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            ..Default::default()
        };
        metadata.set_vendor_record(
            VendorKind::Codex,
            VendorRecord {
                installed_commit: "old".into(),
                path: codex_path,
                mode: InstallMode::Native,
            },
        );
        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            Some("new"),
            "url".into(),
            PlanOfferability::Interactive,
        );
        assert_eq!(plan.vendors.len(), 1);
        assert_eq!(
            plan.vendors[0].reason,
            InstallReason::StaleCommit {
                installed: "old".into(),
                latest: "new".into()
            }
        );
        assert_eq!(plan.latest_known_commit.as_deref(), Some("new"));
    }

    #[test]
    fn build_plan_treats_unknown_installed_commit_as_stale() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("codex");
        create_dir(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, path, InstallMode::Native),
        );
        // Package on disk, no recorded commit, latest known commit set.
        let metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            latest_known_upstream_commit: Some("aaa".into()),
            ..Default::default()
        };
        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            None,
            "url".into(),
            PlanOfferability::Interactive,
        );
        assert_eq!(plan.vendors.len(), 1);
        assert_eq!(
            plan.vendors[0].reason,
            InstallReason::UnknownInstalledCommit {
                latest: "aaa".into()
            }
        );
    }

    #[test]
    fn build_plan_is_empty_when_everything_matches() {
        let dir = TempDir::new().unwrap();
        let codex_path = dir.path().join("codex");
        create_dir(&codex_path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, codex_path.clone(), InstallMode::Native),
        );
        let mut metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            latest_known_upstream_commit: Some("aaa".into()),
            ..Default::default()
        };
        metadata.set_vendor_record(
            VendorKind::Codex,
            VendorRecord {
                installed_commit: "aaa".into(),
                path: codex_path,
                mode: InstallMode::Native,
            },
        );
        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            None,
            "url".into(),
            PlanOfferability::Interactive,
        );
        assert!(plan.is_empty());
        assert_eq!(plan.upstream_check, UpstreamCheck::Skip);
    }

    #[test]
    fn build_plan_propagates_offerability() {
        let targets: BTreeMap<VendorKind, VendorTarget> = BTreeMap::new();
        let metadata = BrainstormMetadata::default();
        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            None,
            "url".into(),
            PlanOfferability::NonInteractive,
        );
        assert_eq!(plan.offerability, PlanOfferability::NonInteractive);
        assert!(plan.is_empty());
    }

    #[test]
    fn build_plan_uses_remote_commit_over_metadata_latest() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("codex");
        create_dir(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, path.clone(), InstallMode::Native),
        );
        let mut metadata = BrainstormMetadata {
            last_checked_at: Some(fresh_within_window(now())),
            latest_known_upstream_commit: Some("old-known".into()),
            ..Default::default()
        };
        metadata.set_vendor_record(
            VendorKind::Codex,
            VendorRecord {
                installed_commit: "old-known".into(),
                path,
                mode: InstallMode::Native,
            },
        );
        // remote returns a newer commit; planner should use it for staleness
        // and surface it in latest_known_commit.
        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            Some("fresh-remote"),
            "url".into(),
            PlanOfferability::Interactive,
        );
        assert_eq!(plan.latest_known_commit.as_deref(), Some("fresh-remote"));
        assert_eq!(plan.vendors.len(), 1);
        match &plan.vendors[0].reason {
            InstallReason::StaleCommit { installed, latest } => {
                assert_eq!(installed, "old-known");
                assert_eq!(latest, "fresh-remote");
            }
            other => panic!("expected StaleCommit, got {other:?}"),
        }
    }

    #[test]
    fn vendors_by_mode_partitions_plan_entries() {
        let dir = TempDir::new().unwrap();
        let codex_path = dir.path().join("never-codex");
        let claude_path = dir.path().join("never-claude");
        let mut targets = BTreeMap::new();
        targets.insert(
            VendorKind::Codex,
            make_target(VendorKind::Codex, codex_path, InstallMode::Native),
        );
        targets.insert(
            VendorKind::Claude,
            make_target(VendorKind::Claude, claude_path, InstallMode::Fallback),
        );
        let metadata = BrainstormMetadata::default();
        let plan = build_plan(
            now(),
            &metadata,
            &targets,
            None,
            "url".into(),
            PlanOfferability::Interactive,
        );
        assert_eq!(plan.vendors_by_mode(InstallMode::Native).len(), 1);
        assert_eq!(plan.vendors_by_mode(InstallMode::Fallback).len(), 1);
    }
}
