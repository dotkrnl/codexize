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
    pub tough: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tasks(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tasks.toml");
        fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[test]
    fn validate_accepts_minimal_tasks_file() {
        let (_dir, path) = write_tasks(
            r#"
[[tasks]]
id = 1
title = "First"
description = "do thing"
test = "cargo test"
estimated_tokens = 100
"#,
        );
        let parsed = validate(&path).unwrap();
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].id, 1);
    }

    #[test]
    fn validate_errors_when_file_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("nope.toml");
        let err = validate(&missing).expect_err("missing file must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("cannot read"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_malformed_toml() {
        let (_dir, path) = write_tasks("not [valid toml");
        let err = validate(&path).expect_err("malformed TOML must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("malformed TOML"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_empty_tasks_list() {
        let (_dir, path) = write_tasks("tasks = []\n");
        let err = validate(&path).expect_err("empty tasks must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("tasks array is empty"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_duplicate_ids() {
        let (_dir, path) = write_tasks(
            r#"
[[tasks]]
id = 7
title = "A"
description = "d"
test = "t"
estimated_tokens = 100

[[tasks]]
id = 7
title = "B"
description = "d"
test = "t"
estimated_tokens = 100
"#,
        );
        let err = validate(&path).expect_err("duplicate id must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("duplicate id 7"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_empty_title() {
        let (_dir, path) = write_tasks(
            r#"
[[tasks]]
id = 1
title = "   "
description = "d"
test = "t"
estimated_tokens = 100
"#,
        );
        let err = validate(&path).expect_err("blank title must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("empty title"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_zero_estimated_tokens() {
        let (_dir, path) = write_tasks(
            r#"
[[tasks]]
id = 1
title = "A"
description = "d"
test = "t"
estimated_tokens = 0
"#,
        );
        let err = validate(&path).expect_err("zero estimated_tokens must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("estimated_tokens must be > 0"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_blank_ref_path() {
        let (_dir, path) = write_tasks(
            r#"
[[tasks]]
id = 1
title = "A"
description = "d"
test = "t"
estimated_tokens = 50

[[tasks.spec_refs]]
path = ""
lines = "1-5"
"#,
        );
        let err = validate(&path).expect_err("blank ref path must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("empty path"), "msg: {msg}");
    }

    #[test]
    fn validate_errors_on_blank_ref_lines() {
        let (_dir, path) = write_tasks(
            r#"
[[tasks]]
id = 1
title = "A"
description = "d"
test = "t"
estimated_tokens = 50

[[tasks.plan_refs]]
path = "plan.md"
lines = "  "
"#,
        );
        let err = validate(&path).expect_err("blank ref lines must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("empty lines"), "msg: {msg}");
    }
}
