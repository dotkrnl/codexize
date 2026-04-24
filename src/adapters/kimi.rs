use super::AgentAdapter;
use std::process::Command;

pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn detect(&self) -> bool {
        Command::new("kimi").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, _model: &str, prompt_path: &str) -> String {
        format!(
            r#"kimi --yolo -p "$(cat {prompt_path})""#,
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, _model: &str, prompt_path: &str) -> String {
        format!(
            r#"kimi --yolo --print < {prompt_path}"#,
            prompt_path = super::shell_escape(prompt_path),
        )
    }
}
