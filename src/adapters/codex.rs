use super::AgentAdapter;
use std::process::Command;

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn detect(&self) -> bool {
        Command::new("codex").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"codex --yolo -m {model} "$(cat {prompt_path})""#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"codex exec --yolo -m {model} - < {prompt_path}"#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
        )
    }
}
