//! Brainstorm sync metadata: TOML-backed record of the upstream commit, the
//! cached source location, and per-vendor installs.
//!
//! Loading is tolerant of missing or malformed files — startup must never
//! panic on a corrupted metadata blob. Callers that need to surface this in
//! status output should use [`load_with_status`].
//!
//! The on-disk schema lives under `~/.codexize/skills/brainstorming/`:
//!
//! ```toml
//! last_checked_at = "2026-04-30T18:54:27Z"
//! upstream_url = "https://github.com/obra/superpowers"
//! latest_known_upstream_commit = "abc123…"
//!
//! [cached_source]
//! commit = "abc123…"
//! path = "/.../cache/superpowers"
//!
//! [vendors.codex]
//! installed_commit = "abc123…"
//! path = "/.../skills/brainstorming"
//! mode = "native"
//! ```

use crate::selection::VendorKind;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Stable string key for a vendor in metadata. Distinct from
/// `vendor_kind_to_str`, which uses model-name conventions ("openai",
/// "google") that would be confusing inside this subsystem's paths.
pub fn vendor_key(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Codex => "codex",
        VendorKind::Claude => "claude",
        VendorKind::Gemini => "gemini",
        VendorKind::Kimi => "kimi",
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    Native,
    Fallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedSource {
    pub commit: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VendorRecord {
    pub installed_commit: String,
    pub path: PathBuf,
    pub mode: InstallMode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrainstormMetadata {
    /// RFC3339 timestamp of the last upstream check. Stored as a string so
    /// malformed values surface to the freshness gate rather than failing
    /// deserialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_known_upstream_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_source: Option<CachedSource>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vendors: BTreeMap<String, VendorRecord>,
}

impl BrainstormMetadata {
    /// Parsed `last_checked_at`. Future timestamps and unparseable strings
    /// return `None` so the freshness gate treats them as expired.
    pub fn parsed_last_checked_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let raw = self.last_checked_at.as_deref()?;
        let parsed = DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc);
        if parsed > now {
            // Future timestamp: spec says treat as expired.
            return None;
        }
        Some(parsed)
    }

    pub fn vendor_record(&self, vendor: VendorKind) -> Option<&VendorRecord> {
        self.vendors.get(vendor_key(vendor))
    }

    pub fn set_vendor_record(&mut self, vendor: VendorKind, record: VendorRecord) {
        self.vendors.insert(vendor_key(vendor).to_string(), record);
    }

    pub fn remove_vendor_record(&mut self, vendor: VendorKind) -> Option<VendorRecord> {
        self.vendors.remove(vendor_key(vendor))
    }

    /// Vendors with a recorded install (regardless of whether the package
    /// still exists on disk). Used to keep previously managed vendors
    /// eligible even when the CLI has temporarily disappeared.
    pub fn recorded_vendors(&self) -> Vec<VendorKind> {
        let mut out = Vec::new();
        for v in [
            VendorKind::Claude,
            VendorKind::Codex,
            VendorKind::Gemini,
            VendorKind::Kimi,
        ] {
            if self.vendors.contains_key(vendor_key(v)) {
                out.push(v);
            }
        }
        out
    }
}

/// Outcome of a metadata load, including a human-readable warning when the
/// file existed but could not be parsed.
#[derive(Debug, Clone)]
pub struct LoadOutcome {
    pub metadata: BrainstormMetadata,
    pub warning: Option<String>,
}

/// Load metadata from `path`. Returns `(default, None)` when the file is
/// missing and `(default, Some(warning))` when it is malformed.
pub fn load_with_status(path: &Path) -> LoadOutcome {
    let raw = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return LoadOutcome {
                metadata: BrainstormMetadata::default(),
                warning: None,
            };
        }
        Err(err) => {
            return LoadOutcome {
                metadata: BrainstormMetadata::default(),
                warning: Some(format!(
                    "failed to read brainstorm metadata at {}: {err}",
                    path.display()
                )),
            };
        }
    };
    match toml::from_str::<BrainstormMetadata>(&raw) {
        Ok(metadata) => LoadOutcome {
            metadata,
            warning: None,
        },
        Err(err) => LoadOutcome {
            metadata: BrainstormMetadata::default(),
            warning: Some(format!(
                "ignoring malformed brainstorm metadata at {}: {err}",
                path.display()
            )),
        },
    }
}

/// Convenience: load metadata, dropping the warning. Use [`load_with_status`]
/// when the caller wants to log corrupt files.
pub fn load(path: &Path) -> BrainstormMetadata {
    load_with_status(path).metadata
}

pub fn save(path: &Path, metadata: &BrainstormMetadata) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create brainstorm metadata directory {}",
                parent.display()
            )
        })?;
    }
    let text = toml::to_string_pretty(metadata)
        .context("failed to serialize brainstorm metadata to TOML")?;
    std::fs::write(path, text)
        .with_context(|| format!("failed to write brainstorm metadata to {}", path.display()))
}

/// Directory under `$HOME` where brainstorm metadata, lock, and cached
/// upstream source live. Errors when `HOME` is unset.
pub fn default_metadata_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home)
        .join(".codexize")
        .join("skills")
        .join("brainstorming"))
}

/// Default path of the metadata TOML file.
pub fn default_metadata_path() -> Result<PathBuf> {
    Ok(default_metadata_dir()?.join("metadata.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn vendor_key_uses_short_lowercase_names() {
        assert_eq!(vendor_key(VendorKind::Codex), "codex");
        assert_eq!(vendor_key(VendorKind::Claude), "claude");
        assert_eq!(vendor_key(VendorKind::Gemini), "gemini");
        assert_eq!(vendor_key(VendorKind::Kimi), "kimi");
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("metadata.toml");
        let outcome = load_with_status(&path);
        assert_eq!(outcome.metadata, BrainstormMetadata::default());
        assert!(outcome.warning.is_none());
    }

    #[test]
    fn load_malformed_toml_returns_default_with_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("metadata.toml");
        write(&path, "not = valid = toml");
        let outcome = load_with_status(&path);
        assert_eq!(outcome.metadata, BrainstormMetadata::default());
        let warn = outcome.warning.expect("malformed file should warn");
        assert!(warn.contains("malformed"), "warning was: {warn}");
    }

    #[test]
    fn load_partial_toml_keeps_known_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("metadata.toml");
        write(
            &path,
            r#"
last_checked_at = "2026-04-30T10:00:00Z"
upstream_url = "https://example.test/super"
"#,
        );
        let outcome = load_with_status(&path);
        assert!(outcome.warning.is_none());
        assert_eq!(
            outcome.metadata.last_checked_at.as_deref(),
            Some("2026-04-30T10:00:00Z")
        );
        assert_eq!(
            outcome.metadata.upstream_url.as_deref(),
            Some("https://example.test/super"),
        );
        assert!(outcome.metadata.vendors.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("metadata.toml");
        let mut metadata = BrainstormMetadata {
            last_checked_at: Some("2026-04-30T10:00:00Z".into()),
            upstream_url: Some("https://example.test/super".into()),
            latest_known_upstream_commit: Some("aaaaaaa".into()),
            cached_source: Some(CachedSource {
                commit: "aaaaaaa".into(),
                path: PathBuf::from("/cache/super"),
            }),
            vendors: BTreeMap::new(),
        };
        metadata.set_vendor_record(
            VendorKind::Codex,
            VendorRecord {
                installed_commit: "aaaaaaa".into(),
                path: PathBuf::from("/home/.codex/skills/brainstorming"),
                mode: InstallMode::Native,
            },
        );
        save(&path, &metadata).unwrap();
        let loaded = load(&path);
        assert_eq!(metadata, loaded);
    }

    #[test]
    fn parsed_last_checked_treats_future_as_expired() {
        let now = Utc.with_ymd_and_hms(2026, 4, 30, 12, 0, 0).unwrap();
        let metadata = BrainstormMetadata {
            last_checked_at: Some("2099-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert!(metadata.parsed_last_checked_at(now).is_none());
    }

    #[test]
    fn parsed_last_checked_treats_garbage_as_expired() {
        let now = Utc.with_ymd_and_hms(2026, 4, 30, 12, 0, 0).unwrap();
        let metadata = BrainstormMetadata {
            last_checked_at: Some("not-a-date".into()),
            ..Default::default()
        };
        assert!(metadata.parsed_last_checked_at(now).is_none());
    }

    #[test]
    fn parsed_last_checked_returns_value_for_past_timestamp() {
        let now = Utc.with_ymd_and_hms(2026, 4, 30, 12, 0, 0).unwrap();
        let earlier = "2026-04-29T10:00:00Z";
        let metadata = BrainstormMetadata {
            last_checked_at: Some(earlier.into()),
            ..Default::default()
        };
        let parsed = metadata.parsed_last_checked_at(now).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-04-29T10:00:00+00:00");
    }

    #[test]
    fn recorded_vendors_lists_only_keys_present_in_metadata() {
        let mut metadata = BrainstormMetadata::default();
        metadata.set_vendor_record(
            VendorKind::Claude,
            VendorRecord {
                installed_commit: "x".into(),
                path: PathBuf::from("/p"),
                mode: InstallMode::Fallback,
            },
        );
        metadata.set_vendor_record(
            VendorKind::Kimi,
            VendorRecord {
                installed_commit: "y".into(),
                path: PathBuf::from("/q"),
                mode: InstallMode::Fallback,
            },
        );
        let recorded: Vec<_> = metadata.recorded_vendors();
        assert_eq!(recorded, vec![VendorKind::Claude, VendorKind::Kimi]);
    }
}
