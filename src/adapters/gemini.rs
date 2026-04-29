use super::{AgentAdapter, CliBinaryAdapter, EffortLevel, prompt_file_subshell};

pub struct GeminiAdapter;

impl CliBinaryAdapter for GeminiAdapter {
    fn binary_name(&self) -> &'static str {
        "gemini"
    }
}

impl AgentAdapter for GeminiAdapter {
    fn detect(&self) -> bool {
        self.detect_cli()
    }

    fn interactive_command(&self, model: &str, prompt_path: &str, _effort: EffortLevel) -> String {
        // -i / --prompt-interactive explicitly executes the prompt and
        // continues in interactive mode (vs a bare positional which could
        // be interpreted as a subcommand name in some shells)
        format!(
            r#"gemini --yolo -m {model} -i {prompt}"#,
            model = super::shell_escape(model),
            prompt = prompt_file_subshell(prompt_path),
        )
    }

    fn noninteractive_command(
        &self,
        model: &str,
        prompt_path: &str,
        _effort: EffortLevel,
    ) -> String {
        // Gemini's plain -p output is readable but lacks any structure for
        // tool calls. stream-json emits init / message / tool_use /
        // tool_result / result events. We format through jq with coloured
        // markers (real ESC bytes via ) and summarise each tool call
        // as one line — description / file_path / path / pattern / command
        // rather than the full parameter blob, truncated to 100 chars.
        let filter = r##"fromjson? | if .type=="init" then "[2;36m▸ " + (.model // "gemini") + "[0m\n" elif .type=="tool_use" then (((.parameters.description // .parameters.file_path // .parameters.path // .parameters.pattern // .parameters.command // "") | tostring | gsub("\n"; " ")) as $s | "\n[1;33m🔧 " + (.tool_name // "tool") + "[0m" + (if $s == "" then "\n" else " [2;33m" + (if ($s | length) > 100 then $s[0:100] + "…" else $s end) + "[0m\n" end)) elif .type=="tool_result" then "[2;" + (if .status=="error" then "31" else "32" end) + "m↳ " + (.status // "done") + "[0m\n" elif .type=="message" and .role=="assistant" then (.content // "") elif .type=="result" then "\n" else "" end"##;
        format!(
            r#"gemini --yolo -m {model} -p {prompt} --output-format stream-json | jq -Rjr --unbuffered '{filter}'"#,
            model = super::shell_escape(model),
            prompt = prompt_file_subshell(prompt_path),
            filter = filter,
        )
    }
}
