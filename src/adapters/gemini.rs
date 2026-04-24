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
        // Gemini's plain -p output is readable but lacks any structure for
        // tool calls. stream-json emits init / message / tool_use /
        // tool_result / result events that we format through jq with
        // coloured markers so tool invocations and streamed assistant text
        // are easy to scan.
        let filter = r##"fromjson? | if .type=="init" then "[2;36m▸ " + (.model // "gemini") + "[0m\n" elif .type=="tool_use" then "\n[1;33m🔧 " + (.tool_name // "tool") + "[0m\n[33m" + ((.parameters // {}) | tostring) + "[0m\n" elif .type=="tool_result" then "[2;32m↳ " + (.status // "done") + "[0m\n" elif .type=="message" and .role=="assistant" then (.content // "") elif .type=="result" then "\n" else "" end"##;
        format!(
            r#"gemini --yolo -m {model} -p "$(cat {prompt_path})" --output-format stream-json | jq -Rjr --unbuffered '{filter}'"#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
            filter = filter,
        )
    }
}
