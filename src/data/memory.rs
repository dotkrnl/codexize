//! Filesystem entry points for project-local memory artifacts.
//!
//! Pure schemas and validation rules live in [`crate::logic::memory`]. This
//! module owns disk reads so the logic layer stays free of backend IO.

pub use crate::logic::memory::{
    DreamChange, DreamChangeKind, DreamReport, DreamStatus, MemoryEntry, MemoryManifest,
    MemoryStatus, MemoryTier, memory_glob_from_session_path, memory_root_from_session_path,
};
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Idempotently seed the project-local memory store with empty index and
/// manifest files so every agent's read path has something to look at on
/// first run. Without this, the cold-start memory directory never exists,
/// the validator's "skip" criterion always fires, and Dreaming never runs.
pub fn ensure_memory_bootstrap(memory_root: &Path) -> Result<()> {
    fs::create_dir_all(memory_root)
        .with_context(|| format!("creating memory root {}", memory_root.display()))?;
    let index = memory_root.join("index.md");
    if !index.exists() {
        fs::write(&index, "# Memory\n\nNo entries yet.\n")
            .with_context(|| format!("seeding memory index {}", index.display()))?;
    }
    let manifest = memory_root.join("manifest.toml");
    if !manifest.exists() {
        fs::write(&manifest, "schema_version = 1\nentries = []\n")
            .with_context(|| format!("seeding memory manifest {}", manifest.display()))?;
    }
    Ok(())
}

pub fn validate_manifest_file(path: &Path) -> Result<MemoryManifest> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read memory manifest {}", path.display()))?;
    let manifest = crate::logic::memory::parse_manifest_toml(&text)
        .with_context(|| format!("malformed memory manifest {}", path.display()))?;
    let root = path
        .parent()
        .with_context(|| format!("memory manifest has no parent: {}", path.display()))?;
    crate::logic::memory::validate_manifest(&manifest, root, Path::is_file)?;
    Ok(manifest)
}

pub fn validate_dream_report_file(path: &Path) -> Result<DreamReport> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read dream report {}", path.display()))?;
    let report = crate::logic::memory::parse_dream_report_toml(&text)
        .with_context(|| format!("malformed dream report {}", path.display()))?;
    let memory_root = path.parent().and_then(Path::parent).with_context(|| {
        format!(
            "dream report is not under memory/dreams: {}",
            path.display()
        )
    })?;
    crate::logic::memory::validate_dream_report(&report, memory_root, Path::is_file)?;
    Ok(report)
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
