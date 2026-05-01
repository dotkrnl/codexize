//! Production [`UpstreamSource`] backed by the local `git` executable, plus a
//! source cache rooted in the codexize-owned brainstorming metadata directory.
//!
//! [`GitUpstream`] resolves the latest commit with `git ls-remote <url> HEAD`
//! and materializes source via a shallow git clone followed by a checkout of
//! the requested commit. Fetched trees are validated to contain
//! `skills/brainstorming/` before any caller sees them; the spec requires that
//! gate before vendor packages can be replaced.
//!
//! [`SourceCache`] persists successfully fetched trees by commit so missing
//! eligible packages can be installed offline when the upstream is briefly
//! unreachable. The cache is a layer above `UpstreamSource`, not part of the
//! trait, so installer/planner code can keep depending on the small trait
//! surface.
//!
//! Network and validation failures are reported as [`UpstreamError`] variants
//! that callers (preflight, planner) can downcast on to choose between
//! "skip silently" and "surface to status".

use super::upstream::UpstreamSource;
use anyhow::Result;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Classified upstream/source-acquisition failures. Wrapped in an
/// `anyhow::Error` to fit the [`UpstreamSource`] trait, but exposed as a
/// concrete enum so call sites can distinguish "network blip, keep going"
/// from "upstream is missing the brainstorming directory, surface this".
#[derive(Debug)]
pub enum UpstreamError {
    /// URL was empty or otherwise rejected before invoking `git`. Treated as
    /// a configuration problem rather than a network failure.
    MalformedUrl(String),
    /// Failed to launch `git` itself (missing executable, permission, ...).
    GitUnavailable(String),
    /// `git ls-remote` exited non-zero or returned an unparseable response.
    LsRemoteFailed { url: String, detail: String },
    /// `git clone` / `git fetch` / `git checkout` failed.
    FetchFailed { url: String, detail: String },
    /// Fetched tree did not contain `skills/brainstorming/`. Distinct from a
    /// transport failure so callers know not to retry.
    MissingBrainstormingDir { url: String, commit: String },
    /// Filesystem operation around the destination directory failed.
    FilesystemFailed(String),
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedUrl(url) => write!(f, "malformed brainstorming upstream URL: {url:?}"),
            Self::GitUnavailable(detail) => write!(f, "git is unavailable: {detail}"),
            Self::LsRemoteFailed { url, detail } => {
                write!(f, "`git ls-remote {url}` failed: {detail}")
            }
            Self::FetchFailed { url, detail } => {
                write!(f, "fetching {url} failed: {detail}")
            }
            Self::MissingBrainstormingDir { url, commit } => write!(
                f,
                "upstream {url}@{commit} does not contain skills/brainstorming/"
            ),
            Self::FilesystemFailed(detail) => write!(f, "filesystem error: {detail}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

/// `UpstreamSource` implementation that shells out to the configured `git`
/// binary. Cheap to construct; safe to share across threads.
#[derive(Debug, Clone)]
pub struct GitUpstream {
    git: PathBuf,
}

impl GitUpstream {
    /// Use whichever `git` is on `PATH`.
    pub fn new() -> Self {
        Self {
            git: PathBuf::from("git"),
        }
    }

    /// Override the git executable path. Useful for tests that want to
    /// pin a specific binary, or for sandboxed environments.
    pub fn with_git_path(git: PathBuf) -> Self {
        Self { git }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.git);
        // `protocol.file.allow=always` lets local-path fixtures work without
        // making the production case any less safe — the protocol filter
        // only applies to local-file URLs, which production never uses.
        cmd.args(["-c", "protocol.file.allow=always"]);
        // Pin cwd to a stable directory: every git invocation here uses
        // absolute URLs/paths, so cwd is irrelevant for correctness, but a
        // deleted inherited cwd (e.g. another parallel test dropped its
        // tempdir) makes git refuse to start with `getcwd` errors.
        cmd.current_dir("/");
        cmd
    }
}

impl Default for GitUpstream {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_url(url: &str) -> Result<&str, UpstreamError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(UpstreamError::MalformedUrl(url.to_string()));
    }
    // Reject leading-dash so we never feed git a flag-like value.
    if trimmed.starts_with('-') {
        return Err(UpstreamError::MalformedUrl(url.to_string()));
    }
    Ok(trimmed)
}

impl UpstreamSource for GitUpstream {
    fn latest_commit(&self, url: &str) -> Result<String> {
        let url = validate_url(url)?;
        let output = self
            .command()
            .args(["ls-remote", "--exit-code", url, "HEAD"])
            .output()
            .map_err(|e| UpstreamError::GitUnavailable(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(UpstreamError::LsRemoteFailed {
                url: url.to_string(),
                detail: if stderr.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    stderr
                },
            }
            .into());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let commit = stdout
            .lines()
            .find_map(|line| line.split_whitespace().next())
            .ok_or_else(|| UpstreamError::LsRemoteFailed {
                url: url.to_string(),
                detail: "empty output".into(),
            })?;
        if !is_plausible_sha(commit) {
            return Err(UpstreamError::LsRemoteFailed {
                url: url.to_string(),
                detail: format!("unexpected ls-remote output: {commit:?}"),
            }
            .into());
        }
        Ok(commit.to_string())
    }

    fn fetch_source(&self, url: &str, commit: &str, dest: &Path) -> Result<()> {
        let url = validate_url(url)?;
        if commit.trim().is_empty() {
            return Err(UpstreamError::FetchFailed {
                url: url.to_string(),
                detail: "empty commit identifier".into(),
            }
            .into());
        }

        // Ensure dest does not exist or is empty — git clone refuses to
        // populate a non-empty directory, and we don't want to mix old
        // contents into a fresh fetch.
        if dest.exists() {
            std::fs::remove_dir_all(dest).map_err(|e| {
                UpstreamError::FilesystemFailed(format!("failed to clear {}: {e}", dest.display()))
            })?;
        }
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                UpstreamError::FilesystemFailed(format!(
                    "failed to create {}: {e}",
                    parent.display()
                ))
            })?;
        }

        // Shallow clone, then checkout the requested commit. When `commit`
        // is the current HEAD (the typical case after `latest_commit`), the
        // initial depth=1 clone is enough; otherwise we deepen with a
        // commit-targeted fetch before checking out.
        let clone_status = self
            .command()
            .args(["clone", "--depth=1", "--quiet", url])
            .arg(dest)
            .output()
            .map_err(|e| UpstreamError::GitUnavailable(e.to_string()))?;
        if !clone_status.status.success() {
            let stderr = String::from_utf8_lossy(&clone_status.stderr)
                .trim()
                .to_string();
            return Err(UpstreamError::FetchFailed {
                url: url.to_string(),
                detail: if stderr.is_empty() {
                    format!("git clone exit status {}", clone_status.status)
                } else {
                    stderr
                },
            }
            .into());
        }

        let head = head_commit(&self.git, dest).map_err(|detail| UpstreamError::FetchFailed {
            url: url.to_string(),
            detail,
        })?;
        if !commit_matches(&head, commit) {
            // Commit isn't HEAD of the default branch: deepen the history
            // for that specific commit and detach onto it.
            let fetch = self
                .command()
                .arg("-C")
                .arg(dest)
                .args(["fetch", "--depth=1", "origin", commit])
                .output()
                .map_err(|e| UpstreamError::GitUnavailable(e.to_string()))?;
            if !fetch.status.success() {
                let stderr = String::from_utf8_lossy(&fetch.stderr).trim().to_string();
                return Err(UpstreamError::FetchFailed {
                    url: url.to_string(),
                    detail: if stderr.is_empty() {
                        format!("git fetch exit status {}", fetch.status)
                    } else {
                        stderr
                    },
                }
                .into());
            }
            let checkout = self
                .command()
                .arg("-C")
                .arg(dest)
                .args(["checkout", "--detach", commit])
                .output()
                .map_err(|e| UpstreamError::GitUnavailable(e.to_string()))?;
            if !checkout.status.success() {
                let stderr = String::from_utf8_lossy(&checkout.stderr).trim().to_string();
                return Err(UpstreamError::FetchFailed {
                    url: url.to_string(),
                    detail: if stderr.is_empty() {
                        format!("git checkout exit status {}", checkout.status)
                    } else {
                        stderr
                    },
                }
                .into());
            }
        }

        if !dest.join("skills").join("brainstorming").is_dir() {
            return Err(UpstreamError::MissingBrainstormingDir {
                url: url.to_string(),
                commit: commit.to_string(),
            }
            .into());
        }
        Ok(())
    }
}

/// Returns the resolved HEAD commit of the working tree at `dir`.
fn head_commit(git: &Path, dir: &Path) -> Result<String, String> {
    let output = Command::new(git)
        .current_dir("/")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Treat `requested` as either a full SHA, a prefix of `actual`, or vice
/// versa. Operators occasionally pass abbreviated SHAs in metadata, and the
/// spec only requires "is this the same commit"-style equality.
fn commit_matches(actual: &str, requested: &str) -> bool {
    if actual.eq_ignore_ascii_case(requested) {
        return true;
    }
    let a = actual.to_ascii_lowercase();
    let r = requested.to_ascii_lowercase();
    a.starts_with(&r) || r.starts_with(&a)
}

fn is_plausible_sha(s: &str) -> bool {
    !s.is_empty() && s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// On-disk cache of upstream source trees, keyed by commit. Lives at
/// `<metadata_dir>/cache/<commit>/`.
///
/// The cache is intentionally simple: we do not refcount, prune, or version.
/// Disk usage stays bounded because there is at most one entry per upstream
/// commit codexize has fetched, and each entry is only the brainstorming
/// repo (small).
#[derive(Debug, Clone)]
pub struct SourceCache {
    root: PathBuf,
}

impl SourceCache {
    /// Place the cache under `metadata_dir/cache/`. `metadata_dir` is
    /// usually [`super::metadata::default_metadata_dir`].
    pub fn new(metadata_dir: &Path) -> Self {
        Self {
            root: metadata_dir.join("cache"),
        }
    }

    /// Path that would hold (or holds) the source tree for `commit`. Does
    /// not check existence; pair with [`Self::hit`] for a presence check.
    pub fn path_for(&self, commit: &str) -> PathBuf {
        self.root.join(commit)
    }

    /// Returns the cached path when it exists *and* contains
    /// `skills/brainstorming/`. A half-written entry from a previous crash
    /// reads as a miss so the caller will refetch.
    pub fn hit(&self, commit: &str) -> Option<PathBuf> {
        let p = self.path_for(commit);
        if p.join("skills").join("brainstorming").is_dir() {
            Some(p)
        } else {
            None
        }
    }

    /// Returns a path containing validated source for `commit`, fetching
    /// through `upstream` only on a cache miss.
    pub fn ensure(
        &self,
        upstream: &dyn UpstreamSource,
        url: &str,
        commit: &str,
    ) -> Result<PathBuf> {
        if let Some(p) = self.hit(commit) {
            return Ok(p);
        }
        let dest = self.path_for(commit);
        upstream.fetch_source(url, commit, &dest)?;
        // Re-check post-fetch in case the implementation lied about
        // success — defense-in-depth for the validation gate.
        if !dest.join("skills").join("brainstorming").is_dir() {
            return Err(UpstreamError::MissingBrainstormingDir {
                url: url.to_string(),
                commit: commit.to_string(),
            }
            .into());
        }
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a local git repo containing `skills/brainstorming/SKILL.md` and
    /// return both its path (usable as an upstream URL) and the HEAD SHA.
    fn make_fixture(brainstorming: bool) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-q", "-b", "main"]);
        run(repo, &["config", "user.email", "fixture@example.test"]);
        run(repo, &["config", "user.name", "Fixture"]);
        if brainstorming {
            let sk = repo.join("skills").join("brainstorming");
            std::fs::create_dir_all(&sk).unwrap();
            std::fs::write(sk.join("SKILL.md"), "# brainstorming\n").unwrap();
        } else {
            std::fs::write(repo.join("README.md"), "no skills here\n").unwrap();
        }
        run(repo, &["add", "."]);
        run(repo, &["commit", "-q", "-m", "fixture"]);
        let head = String::from_utf8(
            Command::new("git")
                .current_dir("/")
                .arg("-C")
                .arg(repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        (dir, head)
    }

    fn run(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir("/")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output();
        let output = output.unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn skip_without_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
    }

    #[test]
    fn latest_commit_succeeds_against_local_fixture() {
        if skip_without_git() {
            return;
        }
        let (fixture, head) = make_fixture(true);
        let upstream = GitUpstream::new();
        let url = fixture.path().to_string_lossy().into_owned();
        let resolved = upstream.latest_commit(&url).unwrap();
        assert_eq!(resolved, head);
    }

    #[test]
    fn latest_commit_fails_for_nonexistent_remote() {
        if skip_without_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let bogus = dir.path().join("does-not-exist");
        let upstream = GitUpstream::new();
        let err = upstream
            .latest_commit(&bogus.to_string_lossy())
            .expect_err("missing remote should fail");
        let downcast = err
            .downcast_ref::<UpstreamError>()
            .expect("errors are typed UpstreamError");
        assert!(
            matches!(downcast, UpstreamError::LsRemoteFailed { .. }),
            "unexpected variant: {downcast:?}"
        );
    }

    #[test]
    fn latest_commit_rejects_malformed_url() {
        let upstream = GitUpstream::new();
        for bad in ["", "   ", "-evil"] {
            let err = upstream.latest_commit(bad).unwrap_err();
            let downcast = err.downcast_ref::<UpstreamError>().unwrap();
            assert!(
                matches!(downcast, UpstreamError::MalformedUrl(_)),
                "{bad:?} should be malformed, got {downcast:?}"
            );
        }
    }

    #[test]
    fn fetch_source_validates_brainstorming_dir() {
        if skip_without_git() {
            return;
        }
        let (fixture, head) = make_fixture(true);
        let dest_root = TempDir::new().unwrap();
        let dest = dest_root.path().join("checkout");
        let upstream = GitUpstream::new();
        upstream
            .fetch_source(&fixture.path().to_string_lossy(), &head, &dest)
            .unwrap();
        assert!(dest.join("skills/brainstorming/SKILL.md").is_file());
    }

    #[test]
    fn fetch_source_rejects_missing_brainstorming_dir() {
        if skip_without_git() {
            return;
        }
        let (fixture, head) = make_fixture(false);
        let dest_root = TempDir::new().unwrap();
        let dest = dest_root.path().join("checkout");
        let upstream = GitUpstream::new();
        let err = upstream
            .fetch_source(&fixture.path().to_string_lossy(), &head, &dest)
            .unwrap_err();
        let downcast = err.downcast_ref::<UpstreamError>().unwrap();
        assert!(
            matches!(downcast, UpstreamError::MissingBrainstormingDir { .. }),
            "{downcast:?}"
        );
    }

    #[test]
    fn fetch_source_clears_preexisting_destination() {
        if skip_without_git() {
            return;
        }
        let (fixture, head) = make_fixture(true);
        let dest_root = TempDir::new().unwrap();
        let dest = dest_root.path().join("checkout");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.txt"), "leftover").unwrap();
        let upstream = GitUpstream::new();
        upstream
            .fetch_source(&fixture.path().to_string_lossy(), &head, &dest)
            .unwrap();
        assert!(!dest.join("stale.txt").exists(), "stale file survived");
        assert!(dest.join("skills/brainstorming/SKILL.md").is_file());
    }

    // Trait stub for cache tests — reusing the production GitUpstream would
    // bring git's process model into a unit-level cache test that only cares
    // about hit/miss bookkeeping.
    struct CountingUpstream {
        commit: String,
        fetches: std::sync::Mutex<usize>,
        place_brainstorming: bool,
    }
    impl UpstreamSource for CountingUpstream {
        fn latest_commit(&self, _url: &str) -> Result<String> {
            Ok(self.commit.clone())
        }
        fn fetch_source(&self, _url: &str, _commit: &str, dest: &Path) -> Result<()> {
            *self.fetches.lock().unwrap() += 1;
            if self.place_brainstorming {
                let sk = dest.join("skills").join("brainstorming");
                std::fs::create_dir_all(&sk).unwrap();
                std::fs::write(sk.join("SKILL.md"), "stub\n").unwrap();
            } else {
                std::fs::create_dir_all(dest).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn cache_miss_invokes_upstream_then_hits_on_repeat() {
        let metadata = TempDir::new().unwrap();
        let cache = SourceCache::new(metadata.path());
        let upstream = CountingUpstream {
            commit: "deadbeefcafef00d".into(),
            fetches: std::sync::Mutex::new(0),
            place_brainstorming: true,
        };
        let path = cache
            .ensure(&upstream, "ignored", "deadbeefcafef00d")
            .unwrap();
        assert!(path.join("skills/brainstorming/SKILL.md").is_file());
        assert_eq!(*upstream.fetches.lock().unwrap(), 1);

        let path2 = cache
            .ensure(&upstream, "ignored", "deadbeefcafef00d")
            .unwrap();
        assert_eq!(path, path2);
        assert_eq!(
            *upstream.fetches.lock().unwrap(),
            1,
            "second ensure should be a hit"
        );
        assert_eq!(cache.hit("deadbeefcafef00d"), Some(path));
        assert_eq!(cache.hit("other-commit"), None);
    }

    #[test]
    fn cache_rejects_fetch_without_brainstorming_dir() {
        let metadata = TempDir::new().unwrap();
        let cache = SourceCache::new(metadata.path());
        let upstream = CountingUpstream {
            commit: "abc1234".into(),
            fetches: std::sync::Mutex::new(0),
            place_brainstorming: false,
        };
        let err = cache.ensure(&upstream, "ignored", "abc1234").unwrap_err();
        let downcast = err.downcast_ref::<UpstreamError>().unwrap();
        assert!(
            matches!(downcast, UpstreamError::MissingBrainstormingDir { .. }),
            "{downcast:?}"
        );
    }

    #[test]
    fn commit_matches_handles_prefixes_and_case() {
        assert!(commit_matches("abcdef1234567890", "abcdef1"));
        assert!(commit_matches("abcdef1", "ABCDEF1234567890"));
        assert!(!commit_matches("abcdef1", "f00d"));
    }
}
