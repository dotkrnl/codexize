//! End-to-end integration tests for `codexize config <subcommand>` and
//! the `codexize ntfy --reset` alias.
//!
//! These drive the same `codexize::data::config::cli` runners that
//! `main.rs` dispatches into, against a tempdir + `CODEXIZE_CONFIG`
//! env override. Rust 1.81+ makes `std::env::set_var` `unsafe`; per the
//! project convention we hold a serial-test guard for the duration of
//! each test so the env mutation doesn't bleed across the suite.

use std::io::Cursor;
use std::path::Path;

use codexize::data::config::cli;
use serial_test::serial;
use tempfile::TempDir;

struct EnvFixture {
    _dir: TempDir,
    config_path: std::path::PathBuf,
    prev: Option<std::ffi::OsString>,
}

impl EnvFixture {
    fn install() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let prev = std::env::var_os("CODEXIZE_CONFIG");
        // SAFETY: serial_test holds the env-mutation lock; Drop restores
        // the previous value so other tests aren't perturbed.
        unsafe {
            std::env::set_var("CODEXIZE_CONFIG", &config_path);
        }
        Self {
            _dir: dir,
            config_path,
            prev,
        }
    }

    fn path(&self) -> &Path {
        &self.config_path
    }
}

impl Drop for EnvFixture {
    fn drop(&mut self) {
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("CODEXIZE_CONFIG", v),
                None => std::env::remove_var("CODEXIZE_CONFIG"),
            }
        }
    }
}

fn buf() -> Cursor<Vec<u8>> {
    Cursor::new(Vec::new())
}

fn s(c: Cursor<Vec<u8>>) -> String {
    String::from_utf8(c.into_inner()).expect("utf8")
}

#[test]
#[serial]
fn config_path_returns_env_override() {
    let fx = EnvFixture::install();
    let mut out = buf();
    cli::run_path(&mut out).unwrap();
    assert_eq!(s(out).trim(), fx.path().to_string_lossy());
}

#[test]
#[serial]
fn config_init_writes_full_defaults_and_round_trips() {
    let fx = EnvFixture::install();
    let mut out = buf();
    cli::run_init(false, &mut out).unwrap();
    assert!(fx.path().exists());

    let mut defaults_buf = buf();
    cli::run_defaults(&mut defaults_buf).unwrap();
    let defaults_text = s(defaults_buf);

    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert_eq!(defaults_text.trim_end(), on_disk.trim_end());

    // Round-trip: the file we wrote loads cleanly.
    let mut validate_buf = buf();
    cli::run_validate(None, &mut validate_buf).unwrap();
    assert_eq!(s(validate_buf).trim(), "ok");
}

#[test]
#[serial]
fn config_init_refuses_overwrite_without_force() {
    let fx = EnvFixture::install();
    std::fs::write(fx.path(), "# pre-existing\n").unwrap();
    let err = cli::run_init(false, &mut buf()).unwrap_err();
    assert!(err.to_string().contains("--force"));
    cli::run_init(true, &mut buf()).expect("--force overwrites");
    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(on_disk.contains("[meta]"));
}

#[test]
#[serial]
fn set_persists_override_and_unset_drops_it() {
    let fx = EnvFixture::install();
    cli::run_set("ntfy.detail_mode", "minimal", &mut buf()).unwrap();
    let mut out = buf();
    cli::run_get("ntfy.detail_mode", &mut out).unwrap();
    assert_eq!(s(out).trim(), "minimal");

    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(on_disk.contains("detail_mode = \"minimal\""));

    cli::run_unset("ntfy.detail_mode", &mut buf()).unwrap();
    let mut out = buf();
    cli::run_get("ntfy.detail_mode", &mut out).unwrap();
    assert_eq!(s(out).trim(), "detailed");

    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(
        !on_disk.contains("detail_mode"),
        "unset must drop the key on disk: {on_disk}"
    );
}

#[test]
#[serial]
fn set_rejects_unknown_key_with_suggestion() {
    let _fx = EnvFixture::install();
    let err = cli::run_set("ntfy.detial_mode", "minimal", &mut buf()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown key"), "{msg}");
    assert!(msg.contains("did you mean 'ntfy.detail_mode'"), "{msg}");
}

#[test]
#[serial]
fn set_invalid_value_reports_error_and_does_not_write_file() {
    let fx = EnvFixture::install();
    let err = cli::run_set("ntfy.retry_attempts", "0", &mut buf()).unwrap_err();
    assert!(err.to_string().contains("retry_attempts"));
    assert!(!fx.path().exists());
}

#[test]
#[serial]
fn set_acp_vendor_disable_round_trip() {
    let fx = EnvFixture::install();
    cli::run_set("acp.agents.kimi.enabled", "false", &mut buf()).unwrap();
    let mut out = buf();
    cli::run_get("acp.agents.kimi.enabled", &mut out).unwrap();
    assert_eq!(s(out).trim(), "false");

    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(on_disk.contains("[acp.agents.kimi]"));
    assert!(on_disk.contains("enabled = false"));
}

#[test]
#[serial]
fn env_pair_set_unset_and_reserved_namespace_rejected() {
    let fx = EnvFixture::install();
    cli::run_set("acp.agents.claude.env.FOO", "bar", &mut buf()).unwrap();
    let mut out = buf();
    cli::run_get("acp.agents.claude.env.FOO", &mut out).unwrap();
    assert_eq!(s(out).trim(), "bar");

    let err = cli::run_set("acp.agents.claude.env.CODEXIZE_ACP_X", "y", &mut buf()).unwrap_err();
    assert!(err.to_string().contains("CODEXIZE_ACP_"));

    cli::run_unset("acp.agents.claude.env.FOO", &mut buf()).unwrap();
    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(
        !on_disk.contains("FOO"),
        "unset must drop the env entry on disk: {on_disk}"
    );
}

#[test]
#[serial]
fn list_section_returns_only_that_subtree() {
    let _fx = EnvFixture::install();
    let mut out = buf();
    cli::run_list(Some("ntfy"), &mut out).unwrap();
    let text = s(out);
    assert!(text.contains("[ntfy]"));
    assert!(text.contains("detail_mode = \"detailed\""));
    assert!(!text.contains("[runner]"));
    assert!(!text.contains("[memory]"));
}

#[test]
#[serial]
fn reset_section_normalizes_to_sparse() {
    let fx = EnvFixture::install();
    cli::run_set("ntfy.detail_mode", "minimal", &mut buf()).unwrap();
    cli::run_set("ntfy.events.stage_wait", "false", &mut buf()).unwrap();
    cli::run_reset(Some("ntfy"), true, true, &mut buf()).unwrap();
    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(!on_disk.contains("detail_mode"));
    assert!(!on_disk.contains("stage_wait"));
}

#[test]
#[serial]
fn reset_no_section_with_yes_deletes_file() {
    let fx = EnvFixture::install();
    cli::run_set("ntfy.detail_mode", "minimal", &mut buf()).unwrap();
    assert!(fx.path().exists());
    cli::run_reset(None, true, true, &mut buf()).unwrap();
    assert!(!fx.path().exists());
}

#[test]
#[serial]
fn reset_no_section_no_yes_non_tty_refuses() {
    let fx = EnvFixture::install();
    cli::run_set("ntfy.detail_mode", "minimal", &mut buf()).unwrap();
    let err = cli::run_reset(None, false, false, &mut buf()).unwrap_err();
    assert!(err.to_string().contains("--yes"));
    assert!(fx.path().exists());
}

#[test]
#[serial]
fn validate_argument_path_surfaces_error_with_position_info() {
    let _fx = EnvFixture::install();
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[ntfy]\nenabled = \"yes\"\n").unwrap();
    let err = cli::run_validate(Some(&bad), &mut buf()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ntfy.enabled"), "{msg}");
    assert!(msg.contains("expected bool"), "{msg}");
}

#[test]
#[serial]
fn ntfy_reset_topic_alias_persists_into_unified_config() {
    let fx = EnvFixture::install();
    let cfg = cli::ntfy_reset_topic().expect("mint topic");
    let topic = cfg.ntfy.topic.value().clone();
    assert_eq!(topic.len(), 32, "32-char hex topic");

    let on_disk = std::fs::read_to_string(fx.path()).unwrap();
    assert!(on_disk.contains(&format!("topic = \"{topic}\"")));
    // Cold-start path: the file must be re-readable.
    let mut buf = buf();
    cli::run_validate(None, &mut buf).unwrap();
    assert_eq!(s(buf).trim(), "ok");
}
