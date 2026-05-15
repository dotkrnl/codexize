use crate::app::{App, ObservedPathState};
use crate::app_runtime::views::split::SplitTargetView as SplitTarget;
use crate::state::{MessageKind, SessionState};
use std::time::{Duration, Instant};
impl App {
    pub(crate) fn observed_path_state(path: &std::path::Path) -> ObservedPathState {
        match std::fs::metadata(path) {
            Ok(meta) => ObservedPathState {
                exists: true,
                modified_at: meta.modified().ok(),
            },
            Err(_) => ObservedPathState {
                exists: false,
                modified_at: None,
            },
        }
    }
    pub(crate) fn update_agent_progress(&mut self) {
        if let Ok(messages) = SessionState::load_messages(&self.state.session_id)
            && messages != self.messages
        {
            self.messages = messages;
        }
        let Some(run) = self.running_run() else {
            self.agent_line_count = 0;
            self.agent_content_hash = 0;
            self.agent_last_change = None;
            return;
        };
        let text = self
            .messages
            .iter()
            .filter(|message| message.run_id == run.id)
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.agent_line_count = text.lines().filter(|l| !l.trim().is_empty()).count();
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        let now = Instant::now();
        if self.agent_content_hash == 0 || hash != self.agent_content_hash {
            self.agent_content_hash = hash;
            self.agent_last_change = Some(now);
            return;
        }
        // Preserve the 30s stall-detector probe; spinner progression is now
        // frame-driven and no longer depends on this branch.
        let _stalled = self
            .agent_last_change
            .map(|last| now.duration_since(last) >= Duration::from_secs(30));
    }
    pub(crate) fn poll_agent_run(&mut self) {
        let Some(run_id) = self.current_run_id else {
            self.pending_drain_deadline = None;
            return;
        };
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            self.pending_drain_deadline = None;
            return;
        };
        if self.active_run_exists(run.id) {
            self.maybe_issue_yolo_exit(&run);
            self.pending_drain_deadline = None;
            return;
        }
        let deadline = *self
            .pending_drain_deadline
            .get_or_insert_with(|| Instant::now() + Self::stamp_timeout_duration());
        let now = Instant::now();
        let stamp_path = self.finish_stamp_path_for(&run);
        let stamp_present = Self::artifact_present(&stamp_path);
        let deadline_elapsed = now >= deadline;
        if !stamp_present && !deadline_elapsed {
            return;
        }
        if !stamp_present && deadline_elapsed && run.stage != "coder" {
            // Reviewer note: fallback warning is emitted once at barrier release
            // so non-coder runs keep their verdict behavior but remain diagnosable.
            self.append_system_message(
                run.id,
                MessageKind::SummaryWarn,
                format!(
                    "finish_stamp_missing: {} (continuing with existing {} verdict logic)",
                    stamp_path.display(),
                    run.stage
                ),
            );
        }
        self.pending_drain_deadline = None;
        self.run_launched = false;
        self.current_run_id = None;
        let outcome = self.finalize_current_run(&run);
        if let Err(err) = outcome {
            self.record_agent_error(err.to_string());
            let _ = self.state.log_event(format!(
                "run finalization failed for {}: {err}",
                run.window_name
            ));
        }
        // Auto-close on exit/stop is interactive-only. Non-interactive runs
        // keep any manually opened split until the operator closes it or a
        // later rebuild evicts it as a stale target.
        if run.modes.interactive && self.split_target == Some(SplitTarget::Run(run.id)) {
            self.close_split();
        }
        self.rebuild_tree_view(None);
    }
}
