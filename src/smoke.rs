use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq)]
pub enum NormalizedArtifact {
    Toml(toml::Value),
    Text(String),
}

pub type NormalizedTree = BTreeMap<String, NormalizedArtifact>;

pub fn live_smoke_prereqs_available() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("TMUX").is_some()
}

pub fn headless_fallback_active() -> bool {
    !live_smoke_prereqs_available()
}

pub fn load_normalized_fixture_tree(root: &Path) -> Result<NormalizedTree> {
    load_tree(root, None)
}

pub fn normalize_session_artifacts(
    root: &Path,
    session_id: &str,
    scratch_root: &str,
) -> Result<NormalizedTree> {
    let replacement = ReplacementSet::new(session_id, scratch_root);
    load_tree(root, Some(&replacement))
}

pub fn diff_normalized_trees(expected: &NormalizedTree, actual: &NormalizedTree) -> Vec<String> {
    let mut diff = Vec::new();
    for path in expected.keys() {
        if !actual.contains_key(path) {
            diff.push(format!("missing artifact: {path}"));
        }
    }
    for path in actual.keys() {
        if !expected.contains_key(path) {
            diff.push(format!("unexpected artifact: {path}"));
        }
    }
    for (path, expected_artifact) in expected {
        let Some(actual_artifact) = actual.get(path) else {
            continue;
        };
        if expected_artifact != actual_artifact {
            diff.push(format!("content mismatch: {path}"));
        }
    }
    diff
}

#[derive(Debug)]
struct ReplacementSet {
    session_id: String,
    scratch_root: String,
    hostname: Option<String>,
}

impl ReplacementSet {
    fn new(session_id: &str, scratch_root: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            scratch_root: scratch_root.to_string(),
            hostname: std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()),
        }
    }
}

fn load_tree(root: &Path, replacements: Option<&ReplacementSet>) -> Result<NormalizedTree> {
    let mut tree = BTreeMap::new();
    collect_tree(root, root, replacements, &mut tree)?;
    Ok(tree)
}

fn collect_tree(
    root: &Path,
    path: &Path,
    replacements: Option<&ReplacementSet>,
    tree: &mut NormalizedTree,
) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("read dir {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("collect dir {}", path.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let entry_path = entry.path();
        if entry.file_type().context("file type")?.is_dir() {
            collect_tree(root, &entry_path, replacements, tree)?;
            continue;
        }

        let rel = entry_path
            .strip_prefix(root)
            .with_context(|| format!("strip prefix {}", entry_path.display()))?;
        let key = normalize_relative_path(rel, replacements);
        let artifact = normalize_file(&entry_path, replacements)?;
        tree.insert(key, artifact);
    }

    Ok(())
}

fn normalize_relative_path(rel: &Path, replacements: Option<&ReplacementSet>) -> String {
    let segments = rel
        .components()
        .map(|component| {
            let raw = component.as_os_str().to_string_lossy();
            if replacements.is_some_and(|set| raw.as_ref() == set.session_id.as_str()) {
                "<SESSION_ID>".to_string()
            } else {
                raw.to_string()
            }
        })
        .collect::<Vec<_>>();
    PathBuf::from_iter(segments).to_string_lossy().into_owned()
}

fn normalize_file(
    path: &Path,
    replacements: Option<&ReplacementSet>,
) -> Result<NormalizedArtifact> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        let mut value: toml::Value =
            toml::from_str(&raw).with_context(|| format!("parse TOML {}", path.display()))?;
        if let Some(set) = replacements {
            normalize_toml_value(None, &mut value, set);
        }
        return Ok(NormalizedArtifact::Toml(value));
    }

    let text = match replacements {
        Some(set) => normalize_text(raw, set),
        None => raw,
    };
    Ok(NormalizedArtifact::Text(text))
}

fn normalize_toml_value(key: Option<&str>, value: &mut toml::Value, replacements: &ReplacementSet) {
    match value {
        toml::Value::String(text) => {
            if is_timestamp_key(key) || looks_like_rfc3339(text) {
                *text = "<TIMESTAMP>".to_string();
            } else if is_env_key(key) {
                *text = "<ENV>".to_string();
            } else {
                *text = normalize_text(text.clone(), replacements);
            }
        }
        toml::Value::Integer(_) | toml::Value::Float(_) => {
            if is_env_key(key) {
                *value = toml::Value::String("<ENV>".to_string());
            }
        }
        toml::Value::Array(items) => {
            for item in items {
                normalize_toml_value(key, item, replacements);
            }
        }
        toml::Value::Table(table) => {
            for (child_key, child_value) in table {
                normalize_toml_value(Some(child_key.as_str()), child_value, replacements);
            }
        }
        _ => {}
    }
}

fn is_timestamp_key(key: Option<&str>) -> bool {
    // Mirrors SessionState/RunRecord timestamp fields plus Message::ts; update
    // this list whenever persisted timestamp columns are added.
    matches!(key, Some("started_at" | "ended_at" | "ts"))
}

fn is_env_key(key: Option<&str>) -> bool {
    // Mirrors env-derived RunRecord fields so real host values do not leak into
    // normalized smoke fixtures when the persisted schema grows.
    matches!(key, Some("hostname" | "mount_device_id"))
}

fn normalize_text(mut text: String, replacements: &ReplacementSet) -> String {
    if !replacements.scratch_root.is_empty() {
        text = text.replace(&replacements.scratch_root, "<ROOT>");
    }
    text = text.replace(&replacements.session_id, "<SESSION_ID>");
    if let Some(hostname) = &replacements.hostname {
        text = text.replace(hostname, "<ENV>");
    }
    normalize_rfc3339_substrings(&text)
}

fn normalize_rfc3339_substrings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut idx = 0;
    while idx < chars.len() {
        if is_timestamp_char(chars[idx]) {
            let start = idx;
            while idx < chars.len() && is_timestamp_char(chars[idx]) {
                idx += 1;
            }
            let candidate: String = chars[start..idx].iter().collect();
            if looks_like_rfc3339(&candidate) {
                out.push_str("<TIMESTAMP>");
            } else {
                out.push_str(&candidate);
            }
        } else {
            out.push(chars[idx]);
            idx += 1;
        }
    }
    out
}

fn looks_like_rfc3339(candidate: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(candidate).is_ok()
}

fn is_timestamp_char(ch: char) -> bool {
    matches!(ch, '0'..='9' | 'T' | 'Z' | ':' | '.' | '+' | '-')
}
