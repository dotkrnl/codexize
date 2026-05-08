//! Filesystem entry points for project-local memory artifacts.
//!
//! Pure schemas and validation rules live in [`crate::logic::memory`]. This
//! module owns disk reads so the logic layer stays free of backend IO.

pub use crate::logic::memory::{
    DreamChange, DreamChangeKind, DreamReport, DreamStatus, MemoryEntry, MemoryManifest,
    MemoryStatus, MemoryTier, memory_glob_from_session_path, memory_root_from_session_path,
};
use anyhow::{Context, Result};
use chrono::{Datelike, Utc};
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

/// Drop journal/<YYYY-MM>.md entries older than `retention_months`.
///
/// `retention_months` is read from `[memory] journal_retention_months`
/// (validated `>= 1`); the loader rejects values below that floor so we
/// can treat the input as positive. The cutoff is "first day of the
/// month that sits exactly `retention_months` ago" — a file dated the
/// same month as the cutoff is kept; older files are removed.
///
/// Returns the number of files removed. Missing journal directory and
/// individual entries with non-`YYYY-MM` filenames are silently
/// preserved — pruning is best-effort and must not delete state the
/// operator dropped in there manually.
pub fn prune_journal_entries(memory_root: &Path, retention_months: u32) -> Result<u32> {
    if retention_months == 0 {
        // Defensive: schema validates >= 1, but treating 0 as "prune everything"
        // would delete the operator's lessons; skip instead so a stale on-disk
        // value can't trigger destructive behavior.
        return Ok(0);
    }
    let journal_dir = memory_root.join("journal");
    if !journal_dir.exists() {
        return Ok(0);
    }
    let now = Utc::now();
    // Cutoff is the year/month that is `retention_months - 1` calendar
    // months before the current month: keep that month and everything
    // after, drop strictly older. Computing in absolute month index keeps
    // the year-rollover case correct.
    let now_index = (now.year() as i64) * 12 + (now.month() as i64 - 1);
    let cutoff_index = now_index - (retention_months as i64) + 1;

    let mut pruned = 0u32;
    let mut dir = match fs::read_dir(&journal_dir) {
        Ok(d) => d,
        Err(_) => return Ok(0),
    };
    while let Some(entry) = dir.next().transpose().ok().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let ext = path.extension().and_then(|s| s.to_str());
        if ext != Some("md") {
            continue;
        }
        let Some((year, month)) = parse_journal_stem(stem) else {
            continue;
        };
        let file_index = (year as i64) * 12 + (month as i64 - 1);
        if file_index < cutoff_index
            && fs::remove_file(&path).is_ok()
        {
            pruned += 1;
        }
    }
    Ok(pruned)
}

fn parse_journal_stem(stem: &str) -> Option<(i32, u32)> {
    // Accept exactly `YYYY-MM` (the journal naming convention used by
    // every memory-aware agent prompt). Any other shape is preserved.
    let mut parts = stem.split('-');
    let year_part = parts.next()?;
    let month_part = parts.next()?;
    if parts.next().is_some() || year_part.len() != 4 || month_part.len() != 2 {
        return None;
    }
    let year: i32 = year_part.parse().ok()?;
    let month: u32 = month_part.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
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
