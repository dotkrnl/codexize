use super::AgentAdapter;
use std::process::Command;

const KIMI_READY_MAX_POLLS: u32 = 50;
const KIMI_READY_POLL_INTERVAL: f32 = 0.2;

pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn detect(&self) -> bool {
        Command::new("kimi")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn interactive_command(&self, _model: &str, prompt_path: &str) -> String {
        // kimi's `-p/--prompt` always exits after the query (one-shot), so we
        // background a tmux paste that polls for TUI readiness before firing,
        // then exec kimi in interactive mode.
        format!(
            concat!(
                r#"(for i in $(seq 1 {max_polls}); do "#,
                r#"tmux capture-pane -p -t "$TMUX_PANE" 2>/dev/null | grep -qE '[❯>]' && break; "#,
                r#"sleep {poll_interval}; "#,
                r#"done && "#,
                r#"tmux load-buffer -b codexize_kimi {prompt_path} && "#,
                r#"tmux paste-buffer -d -b codexize_kimi -t "$TMUX_PANE" && "#,
                r#"tmux send-keys -t "$TMUX_PANE" Enter) & exec kimi --yolo"#,
            ),
            max_polls = KIMI_READY_MAX_POLLS,
            poll_interval = KIMI_READY_POLL_INTERVAL,
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(&self, _model: &str, prompt_path: &str) -> String {
        // `-p` is one-shot and renders its own nicely formatted output — more
        // readable than --print stream-json piped through jq, so use it.
        format!(
            r#"kimi --yolo -p "$(cat {prompt_path})""#,
            prompt_path = super::shell_escape(prompt_path),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_command_polls_for_readiness() {
        let adapter = KimiAdapter;
        let cmd = adapter.interactive_command("any-model", "/tmp/prompt.txt");

        assert!(
            !cmd.contains("sleep 1"),
            "should not use a fixed sleep for startup"
        );
        assert!(
            cmd.contains("capture-pane"),
            "should poll tmux pane content for readiness"
        );
        assert!(cmd.contains("grep"), "should grep for the prompt indicator");
        assert!(cmd.contains("seq 1"), "should loop with bounded retries");
        assert!(
            cmd.contains("exec kimi --yolo"),
            "should still exec kimi in interactive mode"
        );
    }

    #[test]
    fn interactive_command_escapes_prompt_path() {
        let adapter = KimiAdapter;
        let cmd = adapter.interactive_command("m", "/tmp/path with spaces/prompt.txt");

        assert!(
            cmd.contains("'/tmp/path with spaces/prompt.txt'"),
            "should shell-escape paths with spaces"
        );
    }

    #[test]
    fn noninteractive_command_unchanged() {
        let adapter = KimiAdapter;
        let cmd = adapter.noninteractive_command("m", "/tmp/prompt.txt");

        assert_eq!(cmd, r#"kimi --yolo -p "$(cat /tmp/prompt.txt)""#,);
    }
}
