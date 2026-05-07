use anyhow::bail;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

pub const CURRENT_MEMORY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    Hot,
    Warm,
    Cold,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Active,
    Superseded,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub title: String,
    pub topic: String,
    pub file: PathBuf,
    #[serde(default)]
    pub anchor: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    #[serde(default)]
    pub last_dreamed_at: Option<DateTime<Utc>>,
    pub tier: MemoryTier,
    pub status: MemoryStatus,
    pub salience: u8,
    #[serde(default)]
    pub vendors: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DreamStatus {
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamReport {
    pub schema_version: u32,
    pub status: DreamStatus,
    pub summary: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
    #[serde(default)]
    pub changes: Vec<DreamChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DreamChangeKind {
    Promoted,
    Merged,
    Superseded,
    Archived,
    IndexUpdated,
    TierChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamChange {
    pub kind: DreamChangeKind,
    pub target: String,
    pub reason: String,
}

pub fn memory_root_from_session_path(path: &Path) -> PathBuf {
    let absolute = normalize_absolute(path);
    let components: Vec<_> = absolute.components().collect();
    for (i, component) in components.iter().enumerate() {
        if matches!(component, Component::Normal(name) if *name == ".codexize") {
            let mut root = PathBuf::new();
            for component in &components[..=i] {
                root.push(component.as_os_str());
            }
            root.push("memory");
            return root;
        }
    }
    let base = if path.extension().is_some() {
        let parent = absolute.parent().unwrap_or(&absolute);
        // Tests and some helper paths do not live under `.codexize/sessions`;
        // artifact-file fallbacks treat the artifact parent as session-local.
        if parent.file_name().and_then(|name| name.to_str()) == Some("artifacts") {
            parent.parent().unwrap_or(parent)
        } else if parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("rounds")
        {
            parent.parent().and_then(Path::parent).unwrap_or(parent)
        } else {
            parent
        }
    } else {
        absolute.as_path()
    };
    base.join(".codexize").join("memory")
}

pub fn memory_glob_from_session_path(path: &Path) -> PathBuf {
    memory_root_from_session_path(path).join("**")
}

pub fn dream_report_path(memory_root: &Path, round: u32) -> PathBuf {
    memory_root
        .join("dreams")
        .join(format!("dream-{round:04}.toml"))
}

pub fn parse_manifest_toml(text: &str) -> anyhow::Result<MemoryManifest> {
    Ok(toml::from_str(text)?)
}

pub fn validate_manifest(
    manifest: &MemoryManifest,
    memory_root: &Path,
    target_exists: impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    if manifest.schema_version != CURRENT_MEMORY_SCHEMA_VERSION {
        bail!(
            "unsupported memory schema_version {} (expected {})",
            manifest.schema_version,
            CURRENT_MEMORY_SCHEMA_VERSION
        );
    }

    let mut ids = HashSet::new();
    for (i, entry) in manifest.entries.iter().enumerate() {
        validate_entry(i, entry, memory_root, &target_exists)?;
        if !ids.insert(entry.id.as_str()) {
            bail!("entries[{i}]: duplicate id {}", entry.id);
        }
    }

    for (i, entry) in manifest.entries.iter().enumerate() {
        for superseded in &entry.supersedes {
            if !ids.contains(superseded.as_str()) {
                bail!(
                    "entries[{i}]: unknown supersession reference {}",
                    superseded
                );
            }
        }
    }
    reject_supersession_cycles(manifest)
}

fn validate_entry(
    index: usize,
    entry: &MemoryEntry,
    memory_root: &Path,
    target_exists: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    if entry.id.trim().is_empty() {
        bail!("entries[{index}]: empty id");
    }
    if entry.title.trim().is_empty() {
        bail!("entries[{index}]: empty title");
    }
    if entry.topic.trim().is_empty() {
        bail!("entries[{index}]: empty topic");
    }
    if entry.salience == 0 || entry.salience > 5 {
        bail!("entries[{index}]: salience must be 1..5");
    }
    if entry.updated_at < entry.created_at {
        bail!("entries[{index}]: updated_at predates created_at");
    }
    if entry.last_seen_at < entry.created_at {
        bail!("entries[{index}]: last_seen_at predates created_at");
    }
    if let Some(last_dreamed_at) = entry.last_dreamed_at
        && last_dreamed_at < entry.created_at
    {
        bail!("entries[{index}]: last_dreamed_at predates created_at");
    }
    validate_relative_memory_path(index, "file", &entry.file)?;
    let target = memory_root.join(&entry.file);
    if !target_exists(&target) {
        bail!("entries[{index}]: missing target file {}", target.display());
    }
    Ok(())
}

fn is_relative_within_memory(path: &Path) -> bool {
    !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn validate_relative_memory_path(index: usize, field: &str, path: &Path) -> anyhow::Result<()> {
    if !is_relative_within_memory(path) {
        bail!("entries[{index}]: {field} must be relative within .codexize/memory");
    }
    Ok(())
}

fn reject_supersession_cycles(manifest: &MemoryManifest) -> anyhow::Result<()> {
    let graph: HashMap<&str, Vec<&str>> = manifest
        .entries
        .iter()
        .map(|entry| {
            (
                entry.id.as_str(),
                entry.supersedes.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for entry in &manifest.entries {
        if has_cycle(entry.id.as_str(), &graph, &mut visiting, &mut visited) {
            bail!("circular supersession reference involving {}", entry.id);
        }
    }
    Ok(())
}

fn has_cycle<'a>(
    id: &'a str,
    graph: &HashMap<&'a str, Vec<&'a str>>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> bool {
    if !visiting.insert(id) {
        return true;
    }
    if visited.contains(id) {
        let _ = visiting.remove(id);
        return false;
    }
    for next in graph.get(id).into_iter().flatten() {
        if has_cycle(next, graph, visiting, visited) {
            return true;
        }
    }
    let _ = visiting.remove(id);
    visited.insert(id);
    false
}

pub fn parse_dream_report_toml(text: &str) -> anyhow::Result<DreamReport> {
    Ok(toml::from_str(text)?)
}

pub fn validate_dream_report(
    report: &DreamReport,
    memory_root: &Path,
    target_exists: impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    if report.schema_version != CURRENT_MEMORY_SCHEMA_VERSION {
        bail!(
            "unsupported dream schema_version {} (expected {})",
            report.schema_version,
            CURRENT_MEMORY_SCHEMA_VERSION
        );
    }
    if report.summary.trim().is_empty() {
        bail!("dream summary is empty");
    }
    if report.ended_at < report.started_at {
        bail!("dream ended_at predates started_at");
    }
    if report.inputs.is_empty() {
        bail!("dream inputs must not be empty");
    }
    if report.changes.is_empty() {
        bail!("dream changes must not be empty");
    }
    for (i, input) in report.inputs.iter().enumerate() {
        if !is_relative_within_memory(input) {
            bail!("inputs[{i}] must be relative within .codexize/memory");
        }
        let target = memory_root.join(input);
        if !target_exists(&target) {
            bail!("inputs[{i}]: missing input {}", target.display());
        }
    }
    for (i, change) in report.changes.iter().enumerate() {
        if change.target.trim().is_empty() {
            bail!("changes[{i}]: empty target");
        }
        if change.reason.trim().is_empty() {
            bail!("changes[{i}]: empty reason");
        }
    }
    Ok(())
}

pub(crate) fn normalize_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}
