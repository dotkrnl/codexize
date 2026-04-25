use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksFile {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u32,
    pub title: String,
    pub description: String,
    pub test: String,
    pub estimated_tokens: u32,
    #[serde(default)]
    pub spec_refs: Vec<Ref>,
    #[serde(default)]
    pub plan_refs: Vec<Ref>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ref {
    pub path: String,
    pub lines: String,
}

/// Validate a tasks TOML file. Returns parsed structure on success,
/// descriptive error on any structural problem.
pub fn validate(path: &Path) -> Result<TasksFile> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: TasksFile =
        toml::from_str(&text).with_context(|| format!("malformed TOML in {}", path.display()))?;

    if parsed.tasks.is_empty() {
        bail!("tasks array is empty");
    }

    let mut seen_ids = std::collections::BTreeSet::new();
    for (i, t) in parsed.tasks.iter().enumerate() {
        let pos = i + 1;
        if !seen_ids.insert(t.id) {
            bail!("task #{pos}: duplicate id {}", t.id);
        }
        if t.title.trim().is_empty() {
            bail!("task #{pos} (id={}): empty title", t.id);
        }
        if t.description.trim().is_empty() {
            bail!("task #{pos} (id={}): empty description", t.id);
        }
        if t.test.trim().is_empty() {
            bail!("task #{pos} (id={}): empty test", t.id);
        }
        if t.estimated_tokens == 0 {
            bail!("task #{pos} (id={}): estimated_tokens must be > 0", t.id);
        }
        for (j, r) in t.spec_refs.iter().chain(t.plan_refs.iter()).enumerate() {
            if r.path.trim().is_empty() {
                bail!("task #{pos} (id={}): ref[{j}] has empty path", t.id);
            }
            if r.lines.trim().is_empty() {
                bail!("task #{pos} (id={}): ref[{j}] has empty lines", t.id);
            }
        }
    }

    Ok(parsed)
}
