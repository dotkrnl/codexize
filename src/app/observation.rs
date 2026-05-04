// observation.rs
use super::*;
use crate::data::events::DataEvent;
#[cfg(test)]
use crate::data::events::LiveSummaryEvents;
use crate::data::observation::{
    self, LiveSummaryProbe, LiveSummarySnapshot, LiveSummaryWatcher, build_live_summary_watcher,
    ensure_live_summary_watch_dir,
};
use crate::data::runner;
use crate::state::{Message, MessageKind, MessageSender};
use anyhow::Result;

#[cfg(test)]
use std::sync::mpsc;
impl App {
    pub(super) fn setup_watcher(&mut self) -> Result<()> {
        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary_watcher = None;
            self.live_summary_change_events = None;
            return Ok(());
        };

        // Probe parent-directory creation before any test short-circuit so
        // failures still surface as boundary errors with the watcher disabled.
        if let Err(reason) = ensure_live_summary_watch_dir(&path) {
            self.surface_boundary_error(reason, false);
            self.live_summary_watcher = None;
            self.live_summary_change_events = None;
            return Ok(());
        }

        #[cfg(test)]
        if !Self::test_uses_real_live_summary_watcher() {
            let (_tx, rx) = mpsc::channel();
            self.live_summary_watcher = None;
            self.live_summary_change_events = Some(LiveSummaryEvents::new(rx));
            return Ok(());
        }

        match build_live_summary_watcher(&path) {
            LiveSummaryWatcher::Active { watcher, events } => {
                self.live_summary_watcher = Some(watcher);
                self.live_summary_change_events = Some(events);
            }
            LiveSummaryWatcher::PollOnly { reason } => {
                self.surface_boundary_error(reason, false);
                self.live_summary_watcher = None;
                self.live_summary_change_events = None;
            }
            LiveSummaryWatcher::Disabled => {
                self.live_summary_watcher = None;
                self.live_summary_change_events = None;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn test_uses_real_live_summary_watcher() -> bool {
        std::env::var_os("CODEXIZE_TEST_REAL_WATCHER").is_some()
    }

    pub(super) fn process_live_summary_changes(&mut self) {
        // Drain typed `DataEvent::LiveSummaryChanged` notifications from the
        // data-owned watcher seam. Multiple coalesced events still trigger a
        // single re-read because the pipeline is idempotent on repeated
        // reads of the same mtime.
        if let Some(ref events) = self.live_summary_change_events {
            let drained = events.drain();
            if drained
                .iter()
                .any(|event| matches!(event, DataEvent::LiveSummaryChanged))
            {
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
        match observation::probe_live_summary(&path) {
            LiveSummaryProbe::Missing => {
                self.live_summary_cached_text.clear();
                self.live_summary_cached_mtime = None;
            }
            LiveSummaryProbe::Stale => {
                self.live_summary_cached_text.clear();
            }
            LiveSummaryProbe::Fresh { mtime } => {
                let should_read = match self.live_summary_cached_mtime {
                    None => true,
                    Some(cached) => mtime > cached,
                };
                if should_read {
                    self.read_live_summary_pipeline();
                }
            }
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
        // Cheap mtime probe before reading content: avoids the disk read when
        // the file is missing, stale, or unchanged since the last cached
        // mtime. The watchdog still observes the mtime advance below so an
        // unchanged-content write resets the idle clock per spec §3.7.
        let mtime = match observation::probe_live_summary(&path) {
            LiveSummaryProbe::Fresh { mtime } => mtime,
            LiveSummaryProbe::Missing | LiveSummaryProbe::Stale => return,
        };
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
        let Some(LiveSummarySnapshot { content, mtime }) = observation::read_live_summary(&path)
        else {
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

    /// Apply a single drained `DataEvent::ToolCallTransition` to the
    /// matching `WatchdogState`. The drain itself lives in
    /// [`crate::app_runtime::terminal`] so the runtime is the coordinator
    /// that consumes [`DataEvent`]s, and `App` is the per-event handler.
    pub(crate) fn apply_tool_call_transition(
        &mut self,
        window_name: &str,
        transition: crate::data::runner::ToolCallTransition,
    ) {
        let Some(run_id) = self
            .state
            .agent_runs
            .iter()
            .rev()
            .find(|run| run.window_name == window_name)
            .map(|run| run.id)
        else {
            return;
        };
        let Some(state) = self.watchdog.get_mut(run_id) else {
            return;
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
        let prompt_body = observation::read_prompt_body(&prompt_path)
            .unwrap_or_else(|| super::watchdog::PROMPT_UNAVAILABLE_BODY.to_string());
        let warning_text =
            super::watchdog::warning_text(idle_minutes, remaining_minutes, &prompt_body);
        let interrupt_sent = runner::force_interrupt_run_label(&window_name, warning_text);
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
        let terminate_sent = runner::terminate_run_label(&window_name);
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
        // The drain primitive removes the file even when the read fails,
        // so the next run always starts clean.
        if let Some(LiveSummarySnapshot { content, .. }) =
            observation::drain_live_summary_file(&path)
        {
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
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.live_summary_watcher = None;
        self.live_summary_change_events = None;
    }
}
