use super::{AgentAdapter, EffortLevel};
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

    fn interactive_command(&self, model: &str, prompt_path: &str, effort: EffortLevel) -> String {
        let effort_flag = match effort {
            EffortLevel::Low => "--effort low",
            EffortLevel::Normal => "--effort medium",
            EffortLevel::Tough => "--effort max",
        };
        format!(
            r#"claude --dangerously-skip-permissions --model {model} {effort_flag} "$(cat {prompt_path})""#,
            model = super::shell_escape(model),
            effort_flag = effort_flag,
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(
        &self,
        model: &str,
        prompt_path: &str,
        effort: EffortLevel,
    ) -> String {
        // stream-json + --include-partial-messages emits live token/tool/thinking
        // events. The real content events live under `.event` (the top-level
        // record carries `.type == "stream_event"`), so unwrap first. The jq
        // filter formats each type with a coloured marker so text, thinking,
        // and tool use are visually distinct in the tmux pane.
        let effort_flag = match effort {
            EffortLevel::Low => "--effort low",
            EffortLevel::Normal => "--effort medium",
            EffortLevel::Tough => "--effort max",
        };
        let filter = r##"(.event // .) as $e | $e | if .type=="content_block_start" then (if .content_block.type=="thinking" then "\n[2;35m💭 thinking[0m\n[2;35m" elif .content_block.type=="tool_use" then "\n[1;33m🔧 \(.content_block.name)[0m\n[33m" elif .content_block.type=="text" then "[0m\n" else "" end) elif .type=="content_block_delta" then (.delta.text // .delta.thinking // .delta.partial_json // empty) elif .type=="content_block_stop" then "[0m\n" elif .type=="message_stop" then "\n" else empty end"##;
        format!(
            r#"claude --dangerously-skip-permissions --print --output-format stream-json --include-partial-messages --verbose --model {model} {effort_flag} < {prompt_path} | jq -jr --unbuffered '{filter}'"#,
            model = super::shell_escape(model),
            effort_flag = effort_flag,
            prompt_path = super::shell_escape(prompt_path),
            filter = filter,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_command_emits_effort_flag() {
        let adapter = ClaudeAdapter;

        let low =
            adapter.interactive_command("claude-sonnet-4.6", "/tmp/prompt.txt", EffortLevel::Low);
        let normal = adapter.interactive_command(
            "claude-sonnet-4.6",
            "/tmp/prompt.txt",
            EffortLevel::Normal,
        );
        let tough =
            adapter.interactive_command("claude-opus-4.6", "/tmp/prompt.txt", EffortLevel::Tough);

        assert!(low.contains("--effort low"));
        assert!(normal.contains("--effort medium"));
        assert!(tough.contains("--effort max"));
    }

    #[test]
    fn noninteractive_command_emits_effort_flag() {
        let adapter = ClaudeAdapter;

        let low = adapter.noninteractive_command(
            "claude-sonnet-4.6",
            "/tmp/prompt.txt",
            EffortLevel::Low,
        );
        let normal = adapter.noninteractive_command(
            "claude-sonnet-4.6",
            "/tmp/prompt.txt",
            EffortLevel::Normal,
        );
        let tough = adapter.noninteractive_command(
            "claude-opus-4.6",
            "/tmp/prompt.txt",
            EffortLevel::Tough,
        );

        assert!(low.contains("--effort low"));
        assert!(normal.contains("--effort medium"));
        assert!(tough.contains("--effort max"));
    }
}
