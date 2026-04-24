use super::AgentAdapter;
use std::process::Command;

pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn detect(&self) -> bool {
        Command::new("gemini").arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    fn interactive_command(&self, model: &str, prompt_path: &str) -> String {
        // -i / --prompt-interactive explicitly executes the prompt and
        // continues in interactive mode (vs a bare positional which could
        // be interpreted as a subcommand name in some shells)
        format!(
            r#"gemini --yolo -m {model} -i "$(cat {prompt_path})""#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"gemini --yolo -m {model} -p "$(cat {prompt_path})""#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
        )
    }
}
