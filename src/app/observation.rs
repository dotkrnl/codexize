// observation.rs
use super::*;
use crate::state::{Message, MessageKind, MessageSender};
use anyhow::Result;

use notify::Watcher;
use std::sync::mpsc;
impl App {
    pub(super) fn setup_watcher(&mut self) -> Result<()> {
        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary_watcher = None;
            self.live_summary_change_rx = None;
            return Ok(());
        };
        let watch_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        if let Err(e) = std::fs::create_dir_all(&watch_path) {
            self.surface_boundary_error(
                format!("watcher setup failed: {e}, falling back to poll"),
                false,
            );
            self.live_summary_watcher = None;
            self.live_summary_change_rx = None;
            return Ok(());
        }

        #[cfg(test)]
        if !Self::test_uses_real_live_summary_watcher() {
            let (_tx, rx) = mpsc::channel();
            self.live_summary_watcher = None;
            self.live_summary_change_rx = Some(rx);
            return Ok(());
        }

        let (tx, rx) = mpsc::channel();
        let watched_file = path.clone();
        let watcher_result = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res
                    && event.paths.iter().any(|changed| changed == &watched_file)
                {
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        );
        match watcher_result {
            Ok(mut watcher) => {
                if let Err(e) = watcher.watch(&watch_path, notify::RecursiveMode::NonRecursive) {
                    self.surface_boundary_error(
                        format!("watcher setup failed: {e}, falling back to poll"),
                        false,
                    );
                    self.live_summary_watcher = None;
                    self.live_summary_change_rx = None;
                    return Ok(());
                }
                self.live_summary_watcher = Some(watcher);
                self.live_summary_change_rx = Some(rx);
                Ok(())
            }
            Err(e) => {
                self.surface_boundary_error(
                    format!("watcher init failed: {e}, falling back to poll"),
                    false,
                );
                self.live_summary_watcher = None;
                self.live_summary_change_rx = None;
                Ok(())
            }
        }
    }

    #[cfg(test)]
    fn test_uses_real_live_summary_watcher() -> bool {
        std::env::var_os("CODEXIZE_TEST_REAL_WATCHER").is_some()
    }

    pub(super) fn process_live_summary_changes(&mut self) {
        if let Some(ref rx) = self.live_summary_change_rx {
            let mut saw_change = false;
            while rx.try_recv().is_ok() {
                saw_change = true;
            }
            if saw_change {
                self.read_live_summary_pipeline();
            }
        }
        self.poll_live_summary_fallback();
    }

    pub(super) fn poll_live_summary_fallback(&mut self) {
        if !self.run_launched {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary_cached_text.clear();
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            self.live_summary_cached_text.clear();
            return;
        }
        let should_read = match self.live_summary_cached_mtime {
            None => true,
            Some(cached) => mtime > cached,
        };
        if should_read {
            self.read_live_summary_pipeline();
        }
    }

    pub(super) fn read_live_summary_pipeline(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some((run_window_name, run_model, run_vendor)) = self.running_run().map(|run| {
            (
                run.window_name.clone(),
                run.model.clone(),
                run.vendor.clone(),
            )
        }) else {
            return;
        };
        if !self.active_run_exists(&run_window_name) {
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        if let Some(cached_mtime) = self.live_summary_cached_mtime
            && mtime <= cached_mtime
        {
            return;
        }
        // Reset the watchdog idle clock as soon as we observe a real mtime
        // advance, before any later content-staleness or duplicate-content
        // gates (spec §3.7). Empty/duplicate writes still count — the
        // operator-stated heartbeat is the file write itself.
        if let Some(state) = self.watchdog.get_mut(run_id) {
            state.on_live_summary_event(std::time::Instant::now());
        }
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let sanitized = render::sanitize_live_summary(&content);
        if sanitized.is_empty() {
            return;
        }
        if sanitized == self.live_summary_cached_text {
            return;
        }
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: run_model,
                vendor: run_vendor,
            },
            text: sanitized.clone(),
        };
        if let Err(err) = self.state.append_message(&msg) {
            let _ = self.state.log_event(format!(
                "failed to append brief message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(msg);
        }
        self.live_summary_cached_text = sanitized;
        self.live_summary_cached_mtime = Some(mtime);
        // Live-summary deltas are an automatic refocus event, but they are
        // not a re-enable boundary: if the operator has manually navigated
        // away the arrow stays put.
        self.maybe_refocus_to_progress();
    }

    /// Drain runner-emitted tool-call lifecycle transitions and apply them
    /// to the matching `WatchdogState` entries. Called from the main poll
    /// loop alongside `process_live_summary_changes`.
    pub(super) fn apply_pending_tool_call_transitions(&mut self) {
        let transitions = crate::runner::drain_tool_call_transitions();
        if transitions.is_empty() {
            return;
        }
        for (window_name, transition) in transitions {
            let Some(run_id) = self
                .state
                .agent_runs
                .iter()
                .rev()
                .find(|run| run.window_name == window_name)
                .map(|run| run.id)
            else {
                continue;
            };
            let Some(state) = self.watchdog.get_mut(run_id) else {
                continue;
            };
            match transition.kind {
                crate::acp::ToolCallActivityKind::Start => {
                    state.on_tool_call_started(transition.observed_at);
                }
                crate::acp::ToolCallActivityKind::Finish => {
                    state.on_tool_call_finished(transition.observed_at);
                }
            }
        }
    }

    /// Per-tick watchdog evaluation. Walks every registered run's state at
    /// `now`, then performs the spec §3.4/§3.5 side effects (warning
    /// interrupt + `SummaryWarn`, or kill `Terminate` + `SummaryWarn`)
    /// with the App's `&mut self` access to dashboard messages and the
    /// runner registry.
    ///
    /// Decision evaluation and side effects are split into two passes so
    /// the App holds a borrow on the registry only while computing
    /// decisions; the side-effect pass releases that borrow before
    /// reaching back into the same registry to read prompt paths.
    pub(super) fn tick_watchdog(&mut self) {
        if self.watchdog.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let decisions = self.watchdog.evaluate_all(now);
        for (run_id, decision) in decisions {
            match decision {
                super::watchdog::WatchdogDecision::Idle => {}
                super::watchdog::WatchdogDecision::EmitWarning => {
                    self.dispatch_watchdog_warning(run_id, now);
                }
                super::watchdog::WatchdogDecision::EmitKill => {
                    self.dispatch_watchdog_kill(run_id, now);
                }
            }
        }
    }

    fn dispatch_watchdog_warning(&mut self, run_id: u64, now: std::time::Instant) {
        // Snapshot exactly the values we need before touching dashboard
        // helpers. Holding a borrow on `self.watchdog` across calls to
        // `append_system_message` would deadlock the borrow checker.
        let Some((window_name, prompt_path, idle_minutes, remaining_minutes)) =
            self.watchdog.get_mut(run_id).map(|state| {
                (
                    state.window_name.clone(),
                    state.prompt_path.clone(),
                    state.idle_minutes_for_message(now),
                    state.warning_remaining_minutes,
                )
            })
        else {
            return;
        };
        let prompt_body = std::fs::read_to_string(&prompt_path)
            .unwrap_or_else(|_| super::watchdog::PROMPT_UNAVAILABLE_BODY.to_string());
        let warning_text =
            super::watchdog::warning_text(idle_minutes, remaining_minutes, &prompt_body);
        let interrupt_sent = crate::runner::force_interrupt_run_label(&window_name, warning_text);
        let _ = self.state.log_event(format!(
            "watchdog_warning: run={run_id} window={window_name} idle_min={idle_minutes} \
             interrupt_sent={interrupt_sent}"
        ));
        self.append_system_message(
            run_id,
            MessageKind::SummaryWarn,
            format!(
                "watchdog warning: live summary stale for {idle_minutes} min \
                 (tool-call time excluded); sent interrupt with original prompt"
            ),
        );
    }

    fn dispatch_watchdog_kill(&mut self, run_id: u64, now: std::time::Instant) {
        let Some((window_name, idle_minutes)) = self.watchdog.get_mut(run_id).map(|state| {
            (
                state.window_name.clone(),
                state.idle_minutes_for_message(now),
            )
        }) else {
            return;
        };
        // Drop the watchdog state immediately. The runner thread
        // observes the Terminate, exits with code 143, and the existing
        // `poll_agent_run` finalize path drives the standard failed-run
        // vendor failover (spec §3.5). `finalize_run_record` will also
        // call remove() but the duplicate is a no-op.
        self.watchdog.remove(run_id);
        let terminate_sent = crate::runner::terminate_run_label(&window_name);
        let _ = self.state.log_event(format!(
            "watchdog_kill: run={run_id} window={window_name} idle_min={idle_minutes} \
             terminate_sent={terminate_sent}"
        ));
        self.append_system_message(
            run_id,
            MessageKind::SummaryWarn,
            format!(
                "watchdog kill: live summary stale for {idle_minutes} min; \
                 terminating run, vendor failover will follow"
            ),
        );
    }

    /// Final read + cleanup of the live-summary file when a run finishes.
    /// Emits any last summary as a Brief message, then deletes the file so
    /// the next run starts with a clean slate.
    pub(super) fn drain_live_summary(&mut self, run: &crate::state::RunRecord) {
        let path = self.live_summary_path_for(run);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let sanitized = render::sanitize_live_summary(&content);
            if !sanitized.is_empty() && sanitized != self.live_summary_cached_text {
                let msg = Message {
                    ts: chrono::Utc::now(),
                    run_id: run.id,
                    kind: MessageKind::Brief,
                    sender: MessageSender::Agent {
                        model: run.model.clone(),
                        vendor: run.vendor.clone(),
                    },
                    text: sanitized,
                };
                if let Err(err) = self.state.append_message(&msg) {
                    let _ = self.state.log_event(format!(
                        "failed to append final brief message for run {}: {err}",
                        run.id
                    ));
                } else {
                    self.messages.push(msg);
                }
            }
        }
        let _ = std::fs::remove_file(&path);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.live_summary_watcher = None;
        self.live_summary_change_rx = None;
    }
}
