use super::AgentAdapter;
use std::process::Command;

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
        // kimi's `-p/--prompt` always exits after the query (one-shot — see
        // https://moonshotai.github.io/kimi-cli/en/reference/kimi-command.md),
        // and there's no flag to preload a prompt and stay in the TUI. So we
        // background a tmux paste into this pane that fires once the TUI has
        // initialized, then exec kimi in shell (interactive) mode.
        format!(
            r#"(sleep 1 && tmux load-buffer -b codexize_kimi {prompt_path} && tmux paste-buffer -d -b codexize_kimi -t "$TMUX_PANE" && tmux send-keys -t "$TMUX_PANE" Enter) & exec kimi --yolo"#,
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
