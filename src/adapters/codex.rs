use super::{AgentAdapter, CliBinaryAdapter, EffortLevel, prompt_file_subshell};

pub struct CodexAdapter;

impl CliBinaryAdapter for CodexAdapter {
    fn binary_name(&self) -> &'static str {
        "codex"
    }
}

fn effort_flag(effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Low => r#"-c model_reasoning_effort="low""#,
        EffortLevel::Normal => r#"-c model_reasoning_effort="medium""#,
        EffortLevel::Tough => r#"-c model_reasoning_effort="xhigh""#,
    }
}

impl AgentAdapter for CodexAdapter {
    fn detect(&self) -> bool {
        self.detect_cli()
    }

    fn interactive_command(&self, model: &str, prompt_path: &str, effort: EffortLevel) -> String {
        format!(
            r#"codex --yolo -c 'disable_warnings=["model_migrations"]' {effort_flag} -m {model} {prompt}"#,
            model = super::shell_escape(model),
            effort_flag = effort_flag(effort),
            prompt = prompt_file_subshell(prompt_path),
        )
    }

    fn noninteractive_command(
        &self,
        model: &str,
        prompt_path: &str,
        effort: EffortLevel,
    ) -> String {
        // Codex's plain exec renderer repeats full patch diffs after file
        // changes and again at turn end. JSON events let us keep progress
        // readable while summarising file changes as single-line entries.
        let filter = r##"if .type=="item.completed" and .item.type=="agent_message" then "\n[0m" + (.item.text // "") + "\n" elif .type=="item.started" and .item.type=="command_execution" then "\n[1;33m$ [0m" + ((.item.command // "") | gsub("\n"; " ")) + "\n" elif .type=="item.completed" and .item.type=="command_execution" then (if ((.item.aggregated_output // "") | length) > 0 then (.item.aggregated_output // "") + (if ((.item.aggregated_output // "") | endswith("\n")) then "" else "\n" end) else "" end) + "[2;" + (if .item.exit_code==0 then "32" else "31" end) + "m↳ exit " + ((.item.exit_code // "?") | tostring) + "[0m\n" elif .type=="item.completed" and .item.type=="file_change" then ((.item.changes // []) | map("[1;32m✎[0m " + (.path // "<unknown>") + " [2m" + (.kind // "changed") + "[0m") | join("\n")) + "\n" elif .type=="turn.completed" then "\n[2mTokens: " + ((.usage.input_tokens // 0) | tostring) + " in, " + ((.usage.output_tokens // 0) | tostring) + " out[0m\n" else empty end"##;
        format!(
            r#"codex exec --yolo --json {effort_flag} -m {model} - < {prompt_path} 2> >(grep -v '^Reading additional input from stdin\.\.\.$' >&2) | jq -rj --unbuffered '{filter}'"#,
            model = super::shell_escape(model),
            effort_flag = effort_flag(effort),
            prompt_path = super::shell_escape(prompt_path),
            filter = filter,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_command_emits_reasoning_effort_config() {
        let adapter = CodexAdapter;

        let low = adapter.interactive_command("gpt-5.2", "/tmp/prompt.txt", EffortLevel::Low);
        let normal = adapter.interactive_command("gpt-5.2", "/tmp/prompt.txt", EffortLevel::Normal);
        let tough = adapter.interactive_command("gpt-5.2", "/tmp/prompt.txt", EffortLevel::Tough);

        assert!(low.contains(r#"-c model_reasoning_effort="low""#));
        assert!(normal.contains(r#"-c model_reasoning_effort="medium""#));
        assert!(tough.contains(r#"-c model_reasoning_effort="xhigh""#));
    }

    #[test]
    fn noninteractive_command_emits_reasoning_effort_config() {
        let adapter = CodexAdapter;

        let low = adapter.noninteractive_command("gpt-5.2", "/tmp/prompt.txt", EffortLevel::Low);
        let normal =
            adapter.noninteractive_command("gpt-5.2", "/tmp/prompt.txt", EffortLevel::Normal);
        let tough =
            adapter.noninteractive_command("gpt-5.2", "/tmp/prompt.txt", EffortLevel::Tough);

        assert!(low.contains(r#"-c model_reasoning_effort="low""#));
        assert!(normal.contains(r#"-c model_reasoning_effort="medium""#));
        assert!(tough.contains(r#"-c model_reasoning_effort="xhigh""#));
    }
}
