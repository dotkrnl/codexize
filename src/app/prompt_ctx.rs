use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::logic::memory::{memory_root_from_session_path, normalize_absolute};

/// Per-call prompt configuration drawn from the loaded `Config`.
///
/// `max_topics_per_read` is the literal substituted into the
/// `{max_topics_per_read}` slot of the memory_context template.
/// `memory_root` is `Some(_)` when the operator explicitly set
/// `paths.memory_root`, in which case it overrides the session-derived
/// memory location everywhere, including inside the memory-context block.
/// When `None`, `memory_arg` derives the root from the supplied
/// session/artifact path.
#[derive(Debug, Clone, Default)]
pub(crate) struct PromptMeta {
    pub max_topics_per_read: u32,
    pub memory_root: Option<PathBuf>,
}

impl PromptMeta {
    /// Test-only convenience: build a meta with no memory_root override.
    /// Production callers go through `App::prompt_meta()` which captures
    /// the operator's configured override when present.
    #[cfg(test)]
    pub(crate) fn with_topics(max_topics_per_read: u32) -> Self {
        Self {
            max_topics_per_read,
            memory_root: None,
        }
    }
}

pub(super) struct PromptCtx {
    values: HashMap<&'static str, String>,
    max_topics_per_read: u32,
    memory_root_override: Option<PathBuf>,
}
impl PromptCtx {
    pub(super) fn new(meta: PromptMeta) -> Self {
        let mut ctx = Self {
            values: HashMap::new(),
            max_topics_per_read: meta.max_topics_per_read,
            memory_root_override: meta.memory_root,
        };
        ctx.set("project_doc_instr", project_doc_instr());
        ctx
    }
    pub(super) fn set(&mut self, key: &'static str, value: impl Into<String>) -> &mut Self {
        self.values.insert(key, value.into());
        self
    }
    pub(super) fn path(&self, path: impl AsRef<Path>) -> String {
        agent_path(path.as_ref())
    }
    pub(super) fn path_arg(&mut self, key: &'static str, path: impl AsRef<Path>) -> &mut Self {
        self.set(key, self.path(path))
    }
    pub(super) fn ids(&mut self, key: &'static str, ids: &[u32], empty: &str) -> &mut Self {
        let rendered = ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        self.set(
            key,
            if rendered.is_empty() {
                empty.to_string()
            } else {
                rendered
            },
        )
    }
    pub(super) fn live_arg(&mut self, path: impl AsRef<Path>, interactive: bool) -> &mut Self {
        let path = self.path(path);
        let template = if interactive {
            include_str!("prompts/live_summary_interactive.md")
        } else {
            include_str!("prompts/live_summary.md")
        };
        self.set("instr", Self::render_values(template, [("path", path)]))
    }
    pub(super) fn memory_arg(&mut self, session_or_artifact_path: impl AsRef<Path>) -> &mut Self {
        let memory_root = match &self.memory_root_override {
            Some(root) => root.clone(),
            None => memory_root_from_session_path(session_or_artifact_path.as_ref()),
        };
        let rendered = Self::render_values(
            include_str!("prompts/memory_context.md"),
            [
                ("memory_root", self.path(&memory_root)),
                ("memory_index", self.path(memory_root.join("index.md"))),
                (
                    "memory_manifest",
                    self.path(memory_root.join("manifest.toml")),
                ),
                ("max_topics_per_read", self.max_topics_per_read.to_string()),
            ],
        );
        self.set("memory_context", rendered)
    }
    pub(super) fn render(&self, template: &str) -> String {
        Self::render_values(template, self.values.iter().map(|(k, v)| (*k, v.clone())))
    }
    pub(super) fn render_values(
        template: &str,
        values: impl IntoIterator<Item = (&'static str, String)>,
    ) -> String {
        let mut rendered = template.to_string();
        for (key, value) in values {
            // File templates cannot be passed to `formatdoc!`; keep substitution
            // scoped to PromptCtx so wrappers do not rebuild binding arrays.
            rendered = rendered.replace(&format!("{{{key}}}"), &value);
        }
        rendered.replace("{{", "{").replace("}}", "}")
    }
}
pub(super) fn resolved_agent_path(path: &Path) -> PathBuf {
    normalize_absolute(path)
}
fn agent_path(path: &Path) -> String {
    resolved_agent_path(path).display().to_string()
}
fn project_doc_instr() -> String {
    let claude_path = Path::new("CLAUDE.md");
    let agents_path = Path::new("AGENTS.md");
    let docs = match (claude_path.exists(), agents_path.exists()) {
        (true, true) => format!(
            "{} and {}",
            agent_path(claude_path),
            agent_path(agents_path)
        ),
        (true, false) => agent_path(claude_path),
        (false, true) => agent_path(agents_path),
        (false, false) => return String::new(),
    };
    format!("Read {docs} in the repo first and follow those directions carefully.\n\n")
}
#[cfg(test)]
pub(crate) fn live_summary_instruction(path: &Path) -> String {
    let ctx = PromptCtx::new(PromptMeta::with_topics(6));
    PromptCtx::render_values(
        include_str!("prompts/live_summary.md"),
        [("path", ctx.path(path))],
    )
}
#[cfg(test)]
pub(crate) fn live_summary_instruction_interactive(path: &Path) -> String {
    let ctx = PromptCtx::new(PromptMeta::with_topics(6));
    PromptCtx::render_values(
        include_str!("prompts/live_summary_interactive.md"),
        [("path", ctx.path(path))],
    )
}
