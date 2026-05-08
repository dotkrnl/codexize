//! `codexize config <subcommand>` runners.
//!
//! Each public function takes the writable side it needs (stdout, stderr)
//! by reference and returns an `anyhow::Result<()>` — the integration
//! tests drive these directly so the CLI surface is exercised without
//! spawning a subprocess. `main.rs` wires `clap` parsing into a thin
//! dispatch into these runners.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use super::defaults::emit_annotated;
use super::loader::{LoadError, load_from_path, save_atomic_to};
use super::mutate::{
    MutationError, get_value, reset_section, section_dump, set_value, unset_value,
};
use super::paths::config_path;
use super::schema::Config;

/// Effective path the CLI commands operate on. Honors `CODEXIZE_CONFIG`
/// via [`config_path`].
pub fn effective_path() -> PathBuf {
    config_path()
}

/// `codexize config path` — print the resolved config path on its own line.
pub fn run_path(out: &mut dyn Write) -> Result<()> {
    let p = effective_path();
    writeln!(out, "{}", p.display())?;
    Ok(())
}

/// `codexize config defaults` — print the canonical fully-annotated
/// dump of baked defaults to stdout.
pub fn run_defaults(out: &mut dyn Write) -> Result<()> {
    let dump = emit_annotated(&Config::baked_defaults());
    out.write_all(dump.as_bytes())?;
    if !dump.ends_with('\n') {
        writeln!(out)?;
    }
    Ok(())
}

/// `codexize config init [--force]` — write the annotated full-defaults
/// dump to the resolved path. Refuses if the file exists unless `force`.
pub fn run_init(force: bool, out: &mut dyn Write) -> Result<()> {
    let path = effective_path();
    if path.exists() && !force {
        bail!(
            "config: refusing to overwrite existing file at {}; pass --force to overwrite",
            path.display()
        );
    }
    let bytes = emit_annotated(&Config::baked_defaults()).into_bytes();
    crate::data::atomic::atomic_write(&path, &bytes)
        .with_context(|| format!("config: write {}", path.display()))?;
    writeln!(out, "wrote {}", path.display())?;
    Ok(())
}

/// `codexize config list [<section>]` — print the effective config (or
/// just one section) in TOML form. `list` without an argument is the
/// canonical fully-annotated dump applied over the loaded overrides.
pub fn run_list(section: Option<&str>, out: &mut dyn Write) -> Result<()> {
    let path = effective_path();
    let cfg = load_from_path(&path).map_err(load_to_anyhow)?;
    let text = match section {
        Some(s) => section_dump(&cfg, s).ok_or_else(|| anyhow!("config: unknown section '{s}'"))?,
        None => emit_annotated(&cfg),
    };
    out.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        writeln!(out)?;
    }
    Ok(())
}

/// `codexize config get <dotted.key>` — print scalar (one line) or
/// sub-table (TOML) for the requested key.
pub fn run_get(key: &str, out: &mut dyn Write) -> Result<()> {
    let path = effective_path();
    let cfg = load_from_path(&path).map_err(load_to_anyhow)?;
    let value = get_value(&cfg, key).map_err(mutation_to_anyhow)?;
    out.write_all(value.as_bytes())?;
    if !value.ends_with('\n') {
        writeln!(out)?;
    }
    Ok(())
}

/// `codexize config set <dotted.key> <value>` — atomic
/// load → mutate → validate → save (sparse).
pub fn run_set(key: &str, value: &str, out: &mut dyn Write) -> Result<()> {
    let path = effective_path();
    let mut cfg = load_from_path(&path).map_err(load_to_anyhow)?;
    set_value(&mut cfg, key, value).map_err(mutation_to_anyhow)?;
    save_atomic_to(&path, &cfg).map_err(load_to_anyhow)?;
    writeln!(out, "set {key}")?;
    Ok(())
}

/// `codexize config unset <dotted.key>` — drop the override at `key`,
/// rewriting the file in normalized sparse form.
pub fn run_unset(key: &str, out: &mut dyn Write) -> Result<()> {
    let path = effective_path();
    let mut cfg = load_from_path(&path).map_err(load_to_anyhow)?;
    unset_value(&mut cfg, key).map_err(mutation_to_anyhow)?;
    save_atomic_to(&path, &cfg).map_err(load_to_anyhow)?;
    writeln!(out, "unset {key}")?;
    Ok(())
}

/// `codexize config reset [<section>] [--yes]` — with section: drop
/// every override under that section, rewriting the file in normalized
/// sparse form. Without: delete the file entirely (loader's missing-file
/// path then yields baked defaults on next launch). The bare-reset case
/// requires `--yes` on a non-TTY stderr per spec §4.
pub fn run_reset(
    section: Option<&str>,
    yes: bool,
    is_tty_stderr: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let path = effective_path();
    match section {
        Some(name) => {
            let mut cfg = load_from_path(&path).map_err(load_to_anyhow)?;
            reset_section(&mut cfg, name).map_err(mutation_to_anyhow)?;
            save_atomic_to(&path, &cfg).map_err(load_to_anyhow)?;
            writeln!(out, "reset {name}")?;
        }
        None => {
            if !yes && !is_tty_stderr {
                bail!(
                    "config: refusing to delete {} on a non-tty stderr without --yes",
                    path.display()
                );
            }
            if !yes {
                bail!(
                    "config: pass --yes to delete {} (interactive prompt is not implemented)",
                    path.display()
                );
            }
            match std::fs::remove_file(&path) {
                Ok(()) => writeln!(out, "deleted {}", path.display())?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    writeln!(out, "{} already absent", path.display())?
                }
                Err(err) => {
                    return Err(anyhow!(
                        "config: failed to delete {}: {err}",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// `codexize config validate [<path>]` — parse + validate the file at
/// `path` (or [`effective_path`] when `None`). Prints `ok` on success;
/// returns the error verbatim on failure so the caller surfaces it.
/// When the default config path is used and doesn't exist, prints a note
/// to stderr so the operator has a signal that baked defaults were validated
/// rather than their file.
pub fn run_validate(path: Option<&Path>, out: &mut dyn Write) -> Result<()> {
    let resolved: PathBuf = match path {
        Some(p) => p.to_path_buf(),
        None => effective_path(),
    };
    let file_existed = resolved.exists();
    match load_from_path(&resolved) {
        Ok(_) => {
            if !file_existed {
                eprintln!(
                    "note: no file at {}; baked defaults are valid",
                    resolved.display()
                );
            }
            writeln!(out, "ok")?;
            Ok(())
        }
        Err(err) => Err(anyhow!("{err}")),
    }
}

/// `codexize config edit` — spawn `$EDITOR` (fallback `$VISUAL`, then
/// `vi`) on the config path and validate on exit. Per the lower-bound
/// spec contract this is "spawn editor then validate"; the re-edit loop
/// is deferred. On a non-TTY we just validate without spawning an editor.
pub fn run_edit(is_tty_stdin: bool, out: &mut dyn Write) -> Result<()> {
    let path = effective_path();
    if !is_tty_stdin {
        return run_validate(Some(&path), out);
    }
    if !path.exists() {
        bail!(
            "config: {} does not exist; run `codexize config init` first",
            path.display()
        );
    }
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("config: spawn editor {editor:?}"))?;
    if !status.success() {
        bail!("config: editor {editor:?} exited with {status}");
    }
    run_validate(Some(&path), out)
}

/// `codexize ntfy --reset` rewire: mint a fresh topic via the existing
/// `generate_topic` helper, persist it under `[ntfy].topic` in the
/// unified config (creating the file if absent), and return the
/// resulting `Config` so the caller can print whatever surface the user
/// expects. Atomic.
pub fn ntfy_reset_topic() -> Result<Config> {
    let path = effective_path();
    let mut cfg = load_from_path(&path).map_err(load_to_anyhow)?;
    let topic = crate::data::notifications::generate_topic()?;
    let now = chrono::Utc::now();
    if cfg.ntfy.created_at.value().is_none() {
        cfg.ntfy.created_at.set(Some(now));
    }
    cfg.ntfy.updated_at.set(Some(now));
    set_value(&mut cfg, "ntfy.topic", &topic).map_err(mutation_to_anyhow)?;
    save_atomic_to(&path, &cfg).map_err(load_to_anyhow)?;
    Ok(cfg)
}

fn load_to_anyhow(err: LoadError) -> anyhow::Error {
    anyhow!("{err}")
}

fn mutation_to_anyhow(err: MutationError) -> anyhow::Error {
    anyhow!("{err}")
}

#[cfg(test)]
mod tests {
    //! These smoke tests run against `effective_path()` via the
    //! `CODEXIZE_CONFIG` env override; they're serialized so the env
    //! mutation doesn't bleed across the suite.

    use super::*;
    use serial_test::serial;
    use std::io::Cursor;

    struct EnvGuard {
        prev_config: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn install(path: &Path) -> Self {
            let prev_config = std::env::var_os("CODEXIZE_CONFIG");
            // SAFETY: serial_test ensures we hold the global env lock for
            // the duration of this test; the guard's Drop restores the
            // prior value.
            unsafe {
                std::env::set_var("CODEXIZE_CONFIG", path);
            }
            Self { prev_config }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prev_config.take() {
                    Some(v) => std::env::set_var("CODEXIZE_CONFIG", v),
                    None => std::env::remove_var("CODEXIZE_CONFIG"),
                }
            }
        }
    }

    fn out_string() -> Cursor<Vec<u8>> {
        Cursor::new(Vec::new())
    }

    fn into_string(c: Cursor<Vec<u8>>) -> String {
        String::from_utf8(c.into_inner()).expect("utf8 stdout")
    }

    #[test]
    #[serial]
    fn path_honors_codexize_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let mut buf = out_string();
        run_path(&mut buf).unwrap();
        let s = into_string(buf);
        assert_eq!(s.trim(), p.to_string_lossy());
    }

    #[test]
    #[serial]
    fn defaults_and_init_produce_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);

        let mut buf = out_string();
        run_defaults(&mut buf).unwrap();
        let printed = into_string(buf);

        let mut buf = out_string();
        run_init(false, &mut buf).unwrap();
        let on_disk = std::fs::read_to_string(&p).unwrap();

        // `defaults` may pad with one trailing newline; compare trimmed.
        assert_eq!(printed.trim_end(), on_disk.trim_end());
    }

    #[test]
    #[serial]
    fn init_refuses_without_force_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "# placeholder\n").unwrap();
        let _g = EnvGuard::install(&p);
        let mut buf = out_string();
        let err = run_init(false, &mut buf).unwrap_err();
        assert!(err.to_string().contains("--force"));
        run_init(true, &mut buf).expect("init --force overwrites");
    }

    #[test]
    #[serial]
    fn set_then_get_then_unset_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);

        run_set("ntfy.detail_mode", "minimal", &mut out_string()).unwrap();

        let mut buf = out_string();
        run_get("ntfy.detail_mode", &mut buf).unwrap();
        assert_eq!(into_string(buf).trim(), "minimal");

        // The on-disk file must contain the override after set.
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(on_disk.contains("detail_mode = \"minimal\""));

        run_unset("ntfy.detail_mode", &mut out_string()).unwrap();
        let mut buf = out_string();
        run_get("ntfy.detail_mode", &mut buf).unwrap();
        assert_eq!(into_string(buf).trim(), "detailed");

        // After unset the on-disk file no longer carries that key.
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(!on_disk.contains("detail_mode"));
    }

    #[test]
    #[serial]
    fn set_unknown_key_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let err = run_set("ntfy.detial_mode", "minimal", &mut out_string()).unwrap_err();
        assert!(err.to_string().contains("unknown key"));
    }

    #[test]
    #[serial]
    fn set_invalid_value_rejected_by_validate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let err = run_set("ntfy.retry_attempts", "0", &mut out_string()).unwrap_err();
        assert!(err.to_string().contains("retry_attempts"));
        // The file must NOT have been written with the invalid value.
        assert!(!p.exists());
    }

    #[test]
    #[serial]
    fn reset_no_section_with_yes_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[meta]\nversion = 1\n").unwrap();
        let _g = EnvGuard::install(&p);
        run_reset(None, true, true, &mut out_string()).unwrap();
        assert!(!p.exists());
    }

    #[test]
    #[serial]
    fn reset_no_section_no_yes_non_tty_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[meta]\nversion = 1\n").unwrap();
        let _g = EnvGuard::install(&p);
        let err = run_reset(None, false, false, &mut out_string()).unwrap_err();
        assert!(err.to_string().contains("--yes"));
        assert!(p.exists());
    }

    #[test]
    #[serial]
    fn reset_section_clears_overrides_and_normalizes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        run_set("ntfy.detail_mode", "minimal", &mut out_string()).unwrap();
        run_set("ntfy.events.phase_wait", "false", &mut out_string()).unwrap();
        run_reset(Some("ntfy"), true, true, &mut out_string()).unwrap();
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(!on_disk.contains("detail_mode"));
        assert!(!on_disk.contains("phase_wait"));
    }

    #[test]
    #[serial]
    fn validate_default_path_ok() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let mut buf = out_string();
        run_validate(None, &mut buf).unwrap();
        assert_eq!(into_string(buf).trim(), "ok");
    }

    #[test]
    #[serial]
    fn validate_argument_path_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "[ntfy]\nenabled = \"yes\"\n").unwrap();
        let _g = EnvGuard::install(&p);
        let err = run_validate(Some(&bad), &mut out_string()).unwrap_err();
        assert!(err.to_string().contains("expected bool"));
    }

    #[test]
    #[serial]
    fn ntfy_reset_topic_persists_topic_into_config() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let cfg = ntfy_reset_topic().expect("mint topic");
        let topic = cfg.ntfy.topic.value().clone();
        assert_eq!(topic.len(), 32);
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(on_disk.contains(&format!("topic = \"{topic}\"")));
        // Round-trip — loading the file back yields the same topic.
        let reloaded = load_from_path(&p).unwrap();
        assert_eq!(reloaded.ntfy.topic.value(), &topic);
    }

    #[test]
    #[serial]
    fn list_with_section_returns_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let mut buf = out_string();
        run_list(Some("ntfy"), &mut buf).unwrap();
        let s = into_string(buf);
        assert!(s.contains("[ntfy]"));
        assert!(s.contains("detail_mode = \"detailed\""));
        // Must NOT spill into other top-level sections.
        assert!(!s.contains("[runner]"));
    }

    #[test]
    #[serial]
    fn list_unknown_section_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let err = run_list(Some("ntfu"), &mut out_string()).unwrap_err();
        assert!(err.to_string().contains("unknown section"));
    }

    #[test]
    #[serial]
    fn env_pair_set_and_unset_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        run_set("acp.agents.claude.env.FOO", "bar", &mut out_string()).unwrap();
        let mut buf = out_string();
        run_get("acp.agents.claude.env.FOO", &mut buf).unwrap();
        assert_eq!(into_string(buf).trim(), "bar");
        run_unset("acp.agents.claude.env.FOO", &mut out_string()).unwrap();
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(!on_disk.contains("FOO"));
    }

    #[test]
    #[serial]
    fn reserved_env_namespace_rejected_at_set() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);
        let err = run_set(
            "acp.agents.claude.env.CODEXIZE_ACP_X",
            "y",
            &mut out_string(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("CODEXIZE_ACP_"));
        assert!(!p.exists(), "validation failure must not write the file");
    }

    #[test]
    #[serial]
    fn ntfy_reset_preserves_created_at_on_second_reset() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let _g = EnvGuard::install(&p);

        let cfg1 = ntfy_reset_topic().expect("first mint");
        let original_created_at = cfg1.ntfy.created_at.value().clone();
        let topic1 = cfg1.ntfy.topic.value().clone();
        assert!(original_created_at.is_some(), "created_at must be set on first mint");
        assert!(!topic1.is_empty());

        run_unset("ntfy.topic", &mut out_string()).unwrap();
        let cfg2 = ntfy_reset_topic().expect("second mint after unset");
        assert_ne!(cfg2.ntfy.topic.value(), &topic1, "topic should have changed");
        // created_at round-trips through RFC3339 (seconds-only), so compare
        // at second precision rather than expecting nanosecond equality.
        assert_eq!(
            cfg2.ntfy.created_at.value().map(|t| t.timestamp()),
            original_created_at.map(|t| t.timestamp()),
            "created_at must survive a re-mint even after unset"
        );
    }

    #[test]
    #[serial]
    fn validate_missing_default_path_notes_absence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        // The file doesn't exist at the default path.
        assert!(!p.exists());
        let _g = EnvGuard::install(&p);
        let mut buf = out_string();
        // Validates baked defaults (ok) but should note absence on stderr.
        run_validate(None, &mut buf).unwrap();
        assert_eq!(into_string(buf).trim(), "ok");
        // Stderr note is printed via eprintln; we don't capture it here,
        // but the test exercises the ENOENT branch and confirms it succeeds.
    }
}
