//! Injectable boundary for talking to the brainstorming upstream repository.
//!
//! Production code will shell out to `git ls-remote` and a shallow git
//! checkout, but the planner and installer should never know whether source
//! came from the network, a local fixture, or a codexize-owned cache. This
//! file owns just the trait so later layers (and tests) can plug in their
//! own implementations without dragging in git or process spawning.

use anyhow::Result;
use std::path::Path;

/// The default upstream URL when neither metadata nor the
/// `CODEXIZE_BRAINSTORM_UPSTREAM_URL` override is set.
pub const DEFAULT_UPSTREAM_URL: &str = "https://github.com/obra/superpowers";

/// Environment override for the upstream URL. Accepts any URL or local path
/// the configured `git` executable can clone, so tests can point at a
/// throwaway fixture without monkey-patching defaults.
pub const UPSTREAM_URL_ENV: &str = "CODEXIZE_BRAINSTORM_UPSTREAM_URL";

/// Resolve the upstream URL: explicit metadata wins, then the env override,
/// then the project default.
pub fn resolve_upstream_url(configured: Option<&str>) -> String {
    if let Some(url) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        return url.to_string();
    }
    if let Ok(url) = std::env::var(UPSTREAM_URL_ENV)
        && !url.trim().is_empty()
    {
        return url;
    }
    DEFAULT_UPSTREAM_URL.to_string()
}

/// Source of upstream commit/source data. Implementations talk to git;
/// tests use in-memory or local-fixture stand-ins to keep unit tests
/// off the network.
pub trait UpstreamSource: Send + Sync {
    /// Latest commit on the default branch of `url` (typically what
    /// `git ls-remote <url> HEAD` returns). Failures are surfaced so the
    /// caller can degrade gracefully — the spec requires startup to remain
    /// non-blocking when this fails.
    fn latest_commit(&self, url: &str) -> Result<String>;

    /// Materialize the brainstorming source for `commit` from `url` into
    /// `dest`. Implementations should validate that the upstream actually
    /// contains a `skills/brainstorming/` directory before declaring
    /// success; the destination contents are otherwise left unspecified
    /// for the installer to consume.
    fn fetch_source(&self, url: &str, commit: &str, dest: &Path) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Minimal in-memory upstream used by planner/installer tests. The
    /// trait must be object-safe enough to drop into a Box<dyn ...> and
    /// flexible enough to record calls.
    struct StubUpstream {
        latest: Mutex<Result<String, String>>,
        fetched: Mutex<Vec<(String, String, PathBuf)>>,
    }

    impl StubUpstream {
        fn ok(commit: &str) -> Self {
            Self {
                latest: Mutex::new(Ok(commit.to_string())),
                fetched: Mutex::new(Vec::new()),
            }
        }
    }

    impl UpstreamSource for StubUpstream {
        fn latest_commit(&self, _url: &str) -> Result<String> {
            self.latest
                .lock()
                .unwrap()
                .clone()
                .map_err(anyhow::Error::msg)
        }
        fn fetch_source(&self, url: &str, commit: &str, dest: &Path) -> Result<()> {
            self.fetched.lock().unwrap().push((
                url.to_string(),
                commit.to_string(),
                dest.to_path_buf(),
            ));
            Ok(())
        }
    }

    fn _assert_object_safe(_: &dyn UpstreamSource) {}

    #[test]
    fn resolve_upstream_url_prefers_configured_value() {
        let url = resolve_upstream_url(Some("https://example.test/foo"));
        assert_eq!(url, "https://example.test/foo");
    }

    #[test]
    fn resolve_upstream_url_falls_through_blank_configured() {
        // SAFETY: serialized via state::test_fs_lock to avoid concurrent
        // env mutation across tests.
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(UPSTREAM_URL_ENV);
        unsafe { std::env::remove_var(UPSTREAM_URL_ENV) };
        let url = resolve_upstream_url(Some("   "));
        assert_eq!(url, DEFAULT_UPSTREAM_URL);
        if let Some(v) = prev {
            unsafe { std::env::set_var(UPSTREAM_URL_ENV, v) };
        }
    }

    #[test]
    fn resolve_upstream_url_uses_env_override_when_no_configured() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(UPSTREAM_URL_ENV);
        unsafe { std::env::set_var(UPSTREAM_URL_ENV, "https://override.test") };
        let url = resolve_upstream_url(None);
        assert_eq!(url, "https://override.test");
        match prev {
            Some(v) => unsafe { std::env::set_var(UPSTREAM_URL_ENV, v) },
            None => unsafe { std::env::remove_var(UPSTREAM_URL_ENV) },
        }
    }

    #[test]
    fn resolve_upstream_url_default_when_unset() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(UPSTREAM_URL_ENV);
        unsafe { std::env::remove_var(UPSTREAM_URL_ENV) };
        let url = resolve_upstream_url(None);
        assert_eq!(url, DEFAULT_UPSTREAM_URL);
        if let Some(v) = prev {
            unsafe { std::env::set_var(UPSTREAM_URL_ENV, v) };
        }
    }

    #[test]
    fn stub_upstream_records_fetches() {
        let stub = StubUpstream::ok("abcdef0");
        let upstream: &dyn UpstreamSource = &stub;
        assert_eq!(
            upstream.latest_commit("ignored").unwrap(),
            "abcdef0".to_string()
        );
        upstream
            .fetch_source("ignored", "abcdef0", Path::new("/tmp/x"))
            .unwrap();
        let fetched = stub.fetched.lock().unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].1, "abcdef0");
    }
}
