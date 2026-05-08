//! Atomic-write helper for small operator-owned config files.
//!
//! Lifted from the original `notifications.rs::atomic_write_config` so the
//! ntfy publisher and the new unified config loader share one
//! tempfile + rename + `0o600` permissions implementation.

use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` atomically, creating parent directories as
/// needed and (on Unix) setting the file mode to `0o600`.
///
/// The data is first written to a sibling temp file (`.<basename>.<pid>.tmp`)
/// and then `rename(2)`d into place so a partial write or process death
/// never leaves a half-written config on disk.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).context("failed to create config directory")?;
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let tmp_path = dir.join(format!(".{}.{}.tmp", basename, std::process::id()));
    {
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut tmp = options
            .open(&tmp_path)
            .context("failed to create temp config file")?;
        tmp.write_all(bytes)
            .context("failed to write temp config file")?;
        tmp.sync_all().context("failed to sync temp config file")?;
    }
    fs::rename(&tmp_path, path).context("failed to rename temp config file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .context("failed to set config permissions")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_bytes_and_creates_parents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("file.toml");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.toml");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
    }

    #[cfg(unix)]
    #[test]
    fn sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.toml");
        atomic_write(&path, b"x").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
