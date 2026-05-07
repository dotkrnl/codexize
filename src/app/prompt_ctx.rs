use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::logic::memory::{memory_root_from_session_path, normalize_absolute};
pub(super) struct PromptCtx {
    values: HashMap<&'static str, String>,
}
impl PromptCtx {
    pub(super) fn new() -> Self {
        let mut ctx = Self {
            values: HashMap::new(),
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
        let memory_root = memory_root_from_session_path(session_or_artifact_path.as_ref());
        let rendered = Self::render_values(
            include_str!("prompts/memory_context.md"),
            [
                ("memory_root", self.path(&memory_root)),
                ("memory_index", self.path(memory_root.join("index.md"))),
                (
                    "memory_manifest",
                    self.path(memory_root.join("manifest.toml")),
                ),
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
    let ctx = PromptCtx::new();
    PromptCtx::render_values(
        include_str!("prompts/live_summary.md"),
        [("path", ctx.path(path))],
    )
}
#[cfg(test)]
pub(crate) fn live_summary_instruction_interactive(path: &Path) -> String {
    let ctx = PromptCtx::new();
    PromptCtx::render_values(
        include_str!("prompts/live_summary_interactive.md"),
        [("path", ctx.path(path))],
    )
}
