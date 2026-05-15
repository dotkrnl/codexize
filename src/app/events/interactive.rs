use super::super::App;
use super::super::status_line::Severity;
use crate::state::{Message, MessageKind, MessageSender, RunStatus};
use std::time::Duration;
impl App {
    pub(crate) fn interactive_run_active(&self) -> bool {
        let Some(run_id) = self.current_run_id else {
            return false;
        };
        self.state.agent_runs.iter().any(|run| {
            run.id == run_id && run.status == RunStatus::Running && run.modes.interactive
        })
    }
    pub(crate) fn interactive_run_waiting_for_input(&self) -> bool {
        let Some(run_id) = self.current_run_id else {
            return false;
        };
        self.state.agent_runs.iter().any(|run| {
            run.id == run_id
                && run.status == RunStatus::Running
                && run.modes.interactive
                && self.runner_supervisor.run_is_waiting_for_input(run_id)
        })
    }
    pub(crate) fn exit_interactive_run_locally(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        if self.state.agent_runs.iter().any(|run| run.id == run_id) {
            // Push the FSM into `Stopping(GoIdle)` so
            // `handle_run_finalization_failure` recognizes the stop intent
            // and short-circuits the auto-retry path. Without this, an
            // exit_code=1 from `/exit` during human-blocked recovery (e.g.
            // before recovery.toml was written) would route through
            // `maybe_auto_retry` and silently relaunch a new agent instead
            // of stopping.
            self.sync_fsm_running_state();
            let _ = self.fsm.request_stop(crate::lifecycle::AfterStop::GoIdle);
            // `/exit` is a local codexize control for interactive ACP runs,
            // not agent prompt text, so the runner completes this run by id.
            self.runner_supervisor.request_run_exit(run_id);
        }
    }
    pub(crate) fn send_interactive_input(&mut self, input: String) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        if !self.state.agent_runs.iter().any(|run| run.id == run_id) {
            return;
        };
        if self.runner_supervisor.send_run_input(run_id, input.clone()) {
            let message = Message {
                ts: chrono::Utc::now(),
                run_id,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: input,
            };
            if let Err(err) = self.state.append_message(&message) {
                let _ = self.state.log_event(format!(
                    "failed to append user input for run {run_id}: {err}"
                ));
            } else {
                self.messages.push(message);
                self.agent_last_change = Some(std::time::Instant::now());
            }
        } else {
            self.push_status(
                "interactive agent is not ready for input".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
        }
    }
    pub(crate) fn append_user_input_message(&mut self, run_id: u64, input: String) {
        let message = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::UserInput,
            sender: MessageSender::System,
            text: input,
        };
        if let Err(err) = self.state.append_message(&message) {
            let _ = self.state.log_event(format!(
                "failed to append user input for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(message);
            self.agent_last_change = Some(std::time::Instant::now());
        }
    }
    pub(crate) fn interrupt_interactive_input(&mut self, input: String) {
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            self.push_status(
                "interrupt requires a message".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        }
        let Some(run_id) = self.current_run_id else {
            return;
        };
        if !self.state.agent_runs.iter().any(|run| run.id == run_id) {
            return;
        };
        if self
            .runner_supervisor
            .interrupt_run_input(run_id, trimmed.clone())
        {
            self.append_user_input_message(run_id, trimmed);
        } else {
            self.push_status(
                "no running agent to interrupt".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
        }
    }
    pub(crate) fn toggle_noninteractive_texts(&mut self) {
        self.state.show_noninteractive_texts = !self.state.show_noninteractive_texts;
        let label = if self.state.show_noninteractive_texts {
            "showing non-interactive agent text"
        } else {
            "hiding non-interactive agent text"
        };
        let _ = self.state.log_event(format!(
            "show_noninteractive_texts={}",
            self.state.show_noninteractive_texts
        ));
        if let Err(err) = self.state.save() {
            self.push_status(
                format!("texts: failed to save setting: {err}"),
                Severity::Error,
                Duration::from_secs(5),
            );
        } else {
            self.push_status(label.to_string(), Severity::Info, Duration::from_secs(3));
        }
    }
    pub(crate) fn toggle_thinking_texts(&mut self) {
        self.state.show_thinking_texts = !self.state.show_thinking_texts;
        let label = if self.state.show_thinking_texts {
            "showing thinking text"
        } else {
            "hiding thinking text"
        };
        let _ = self.state.log_event(format!(
            "show_thinking_texts={}",
            self.state.show_thinking_texts
        ));
        if let Err(err) = self.state.save() {
            self.push_status(
                format!("verbose: failed to save setting: {err}"),
                Severity::Error,
                Duration::from_secs(5),
            );
            return;
        }
        self.push_status(label.to_string(), Severity::Info, Duration::from_secs(3));
    }
}
#[cfg(test)]
#[path = "interactive_tests.rs"]
mod tests;
