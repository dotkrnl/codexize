use super::AgentAdapter;
use std::process::Command;

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn detect(&self) -> bool {
        Command::new("claude")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn interactive_command(&self, model: &str, prompt_path: &str) -> String {
        format!(
            r#"claude --dangerously-skip-permissions --model {model} "$(cat {prompt_path})""#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, model: &str, prompt_path: &str) -> String {
        // stream-json + --include-partial-messages emits live token/tool/thinking
        // events. The real content events live under `.event` (the top-level
        // record carries `.type == "stream_event"`), so unwrap first. The jq
        // filter formats each type with a coloured marker so text, thinking,
        // and tool use are visually distinct in the tmux pane.
        let filter = r##"(.event // .) as $e | $e | if .type=="content_block_start" then (if .content_block.type=="thinking" then "\n[2;35m💭 thinking[0m\n[2;35m" elif .content_block.type=="tool_use" then "\n[1;33m🔧 \(.content_block.name)[0m\n[33m" elif .content_block.type=="text" then "[0m\n" else "" end) elif .type=="content_block_delta" then (.delta.text // .delta.thinking // .delta.partial_json // empty) elif .type=="content_block_stop" then "[0m\n" elif .type=="message_stop" then "\n" else empty end"##;
        format!(
            r#"claude --dangerously-skip-permissions --print --output-format stream-json --include-partial-messages --verbose --model {model} < {prompt_path} | jq -jr --unbuffered '{filter}'"#,
            model = super::shell_escape(model),
            prompt_path = super::shell_escape(prompt_path),
            filter = filter,
        )
    }
}
