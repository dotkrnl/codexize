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
        // kimi's default `--print` emits Python-repr-style event objects that
        // are unreadable; stream-json instead gives a single final message
        // object with typed content blocks. jq formats think / text / tool
        // use blocks with coloured markers. `fromjson?` skips non-JSON
        // trailers like the "To resume this session:" line.
        let filter = r##"fromjson? | .content[]? | if .type=="think" then "\n[2;35m💭 thinking[0m\n[2;35m" + (.think // "") + "[0m\n" elif .type=="text" then (.text // "") + "\n" elif .type=="tool_use" then "\n[1;33m🔧 " + (.name // "tool") + "[0m\n[33m" + ((.arguments // .input // "") | tostring) + "[0m\n" else "" end"##;
        format!(
            r#"kimi --yolo --print --output-format stream-json < {prompt_path} | jq -Rjr --unbuffered '{filter}'"#,
            prompt_path = super::shell_escape(prompt_path),
            filter = filter,
        )
    }
}
