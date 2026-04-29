use super::{AgentAdapter, CliBinaryAdapter, EffortLevel, prompt_file_subshell};

const KIMI_READY_MAX_POLLS: u32 = 50;
const KIMI_READY_POLL_INTERVAL: f32 = 0.2;
const KIMI_READY_INITIAL_DELAY: f32 = 1.5;
const KIMI_READY_SETTLE_DELAY: f32 = 1.0;

pub struct KimiAdapter;

impl CliBinaryAdapter for KimiAdapter {
    fn binary_name(&self) -> &'static str {
        "kimi"
    }
}

impl AgentAdapter for KimiAdapter {
    fn detect(&self) -> bool {
        self.detect_cli()
    }

    fn interactive_command(&self, _model: &str, prompt_path: &str, effort: EffortLevel) -> String {
        // kimi's `-p/--prompt` always exits after the query (one-shot), so we
        // background a tmux paste that polls for TUI readiness before firing,
        // then exec kimi in interactive mode.
        // The readiness glyphs are based on Kimi's current TUI prompt; if they
        // change, the bounded loop still falls back to pasting after timeout.
        let _ = effort;
        format!(
            concat!(
                r#"(sleep {initial_delay}; for i in $(seq 1 {max_polls}); do "#,
                r#"tmux capture-pane -p -t "$TMUX_PANE" 2>/dev/null | grep -q ' input ' && break; "#,
                r#"sleep {poll_interval}; "#,
                r#"done && sleep {settle_delay:.1} && "#,
                r#"{{ cat {prompt_path}; printf '\n'; }} | tmux load-buffer -b codexize_kimi - && "#,
                r#"tmux paste-buffer -p -r -d -b codexize_kimi -t "$TMUX_PANE") & exec kimi --yolo --thinking"#,
            ),
            max_polls = KIMI_READY_MAX_POLLS,
            poll_interval = KIMI_READY_POLL_INTERVAL,
            initial_delay = KIMI_READY_INITIAL_DELAY,
            settle_delay = KIMI_READY_SETTLE_DELAY,
            prompt_path = super::shell_escape(prompt_path),
        )
    }

    fn noninteractive_command(
        &self,
        _model: &str,
        prompt_path: &str,
        effort: EffortLevel,
    ) -> String {
        // `-p` is one-shot and renders its own nicely formatted output — more
        // readable than --print stream-json piped through jq, so use it.
        // Kimi without --thinking produces low-quality output, so always enable it.
        let _ = effort;
        format!(
            r#"kimi --yolo --thinking -p {prompt}"#,
            prompt = prompt_file_subshell(prompt_path),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_command_polls_for_readiness() {
        let adapter = KimiAdapter;
        let cmd = adapter.interactive_command("any-model", "/tmp/prompt.txt", EffortLevel::Normal);

        assert!(
            cmd.contains("sleep 1.5"),
            "should wait briefly before polling so kimi can initialise"
        );
        assert!(
            cmd.contains("capture-pane"),
            "should poll tmux pane content for readiness"
        );
        assert!(
            cmd.contains("grep -q ' input '"),
            "should grep for the input box label"
        );
        assert!(
            cmd.contains("sleep 1.0"),
            "should settle after readiness detection"
        );
        assert!(
            cmd.contains("printf '\\n'"),
            "should ensure pasted prompt submits via trailing newline"
        );
        assert!(
            cmd.contains("tmux load-buffer -b codexize_kimi -"),
            "should load normalized prompt content from stdin"
        );
        assert!(cmd.contains("seq 1"), "should loop with bounded retries");
        assert!(
            cmd.contains("tmux paste-buffer -p -r"),
            "should use bracketed paste with raw LF to prevent multi-line input chunking"
        );
        assert!(
            !cmd.contains(r#"tmux send-keys -t "$TMUX_PANE" Enter"#),
            "should not send a separate Enter after bracketed paste"
        );
        assert!(
            cmd.contains("exec kimi --yolo"),
            "should still exec kimi in interactive mode"
        );
    }

    #[test]
    fn interactive_command_escapes_prompt_path() {
        let adapter = KimiAdapter;
        let cmd = adapter.interactive_command(
            "m",
            "/tmp/path with spaces/prompt.txt",
            EffortLevel::Normal,
        );

        assert!(
            cmd.contains("'/tmp/path with spaces/prompt.txt'"),
            "should shell-escape paths with spaces"
        );
    }

    #[test]
    fn noninteractive_command_always_enables_thinking() {
        let adapter = KimiAdapter;
        let cmd = adapter.noninteractive_command("m", "/tmp/prompt.txt", EffortLevel::Normal);

        assert_eq!(cmd, r#"kimi --yolo --thinking -p "$(cat /tmp/prompt.txt)""#,);
    }

    #[test]
    fn commands_always_enable_thinking_regardless_of_effort() {
        let adapter = KimiAdapter;

        let normal = adapter.interactive_command("m", "/tmp/prompt.txt", EffortLevel::Normal);
        let tough = adapter.noninteractive_command("m", "/tmp/prompt.txt", EffortLevel::Tough);

        assert!(normal.contains("exec kimi --yolo --thinking"));
        assert!(!normal.contains("--no-thinking"));
        assert!(tough.contains("kimi --yolo --thinking"));
        assert!(!tough.contains("--no-thinking"));
    }
}
