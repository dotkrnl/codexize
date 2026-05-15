use super::*;
use crate::app::render_helpers as render;
/// Pure helper: decide whether a freshly-sanitized live-summary payload
/// represents real operator progress (i.e. should reset the watchdog idle
/// clock) given the last cached payload. Empty or duplicate payloads return
/// false — a bare mtime touch with no semantic delta must not be treated as
/// a heartbeat, otherwise an agent that periodically re-flushes the same
/// summary while actually hung slips past the warn threshold.
pub(crate) fn live_summary_advances_content(sanitized: &str, cached: &str) -> bool {
    !sanitized.is_empty() && sanitized != cached
}
#[cfg(test)]
use crate::data::events::LiveSummaryEvents;
use crate::data::observation::{
    self, LiveSummaryProbe, LiveSummarySnapshot, LiveSummaryWatcher, build_live_summary_watcher,
    ensure_live_summary_watch_dir,
};
use crate::state::{Message, MessageKind, MessageSender};
#[cfg(test)]
use tokio::sync::mpsc;
impl App {
    pub(crate) fn setup_watcher(&mut self) {
        self.live_summary_watcher = None;
        self.live_summary_change_events = None;
        let Some(path) = self.live_summary_path.clone() else {
            return;
        };
        // Probe parent-directory creation before any test short-circuit so
        // failures still surface as boundary errors with the watcher disabled.
        if let Err(reason) = ensure_live_summary_watch_dir(&path) {
            self.surface_boundary_error(reason, false);
            return;
        }
        #[cfg(test)]
        if !Self::test_uses_real_live_summary_watcher() {
            let (_tx, rx) = mpsc::unbounded_channel();
            self.live_summary_change_events = Some(LiveSummaryEvents::new(rx));
            return;
        }
        match build_live_summary_watcher(&path) {
            LiveSummaryWatcher::Active { watcher, events } => {
                self.live_summary_watcher = Some(watcher);
                self.live_summary_change_events = Some(events);
            }
            LiveSummaryWatcher::PollOnly { reason } => {
                self.surface_boundary_error(reason, false);
            }
            LiveSummaryWatcher::Disabled => {}
        }
    }
    #[cfg(test)]
    fn test_uses_real_live_summary_watcher() -> bool {
        std::env::var_os("CODEXIZE_TEST_REAL_WATCHER").is_some()
    }
    /// Install the cache watcher rooted at `paths.cache_root`. Idempotent
    /// — calling it again tears down the previous watcher first. Failures
    /// to install the notify backend are non-fatal: the watcher's 60-s
    /// mtime poll still runs and the reason is surfaced as a boundary log
    /// so the operator can investigate (matches the live-summary watcher
    /// degradation path).
    pub(crate) fn setup_cache_watcher(&mut self) {
        self.cache_watcher = None;
        let dir = self.paths.cache_root.clone();
        // Skip the real notify watcher in unit tests by default to keep
        // the in-process tests hermetic (the live-summary watcher uses
        // the same convention). Opt in by setting CODEXIZE_TEST_REAL_WATCHER.
        #[cfg(test)]
        if !Self::test_uses_real_live_summary_watcher() {
            return;
        }
        let initial_mtime = cache::watcher::current_mtime(&dir.join("models.json"));
        match cache::CacheWatcher::start(&dir, initial_mtime) {
            cache::CacheWatcherOutcome::Active(watcher) => {
                self.cache_watcher = Some(watcher);
            }
            cache::CacheWatcherOutcome::PollOnly { reason, watcher } => {
                self.cache_watcher = Some(watcher);
                self.surface_boundary_error(reason, false);
            }
        }
    }
    /// Drain pending cache-watcher signals and, on an external publish,
    /// reload the on-disk cache and refresh the model strip. Emits a
    /// `cache_external_publish_observed` event so the publisher/follower
    /// telemetry stays symmetric with the rest of the cache layer.
    pub(crate) fn poll_cache_watcher(&mut self) {
        let Some(watcher) = self.cache_watcher.as_mut() else {
            return;
        };
        if !watcher.poll() {
            return;
        }
        let cache_path = watcher.cache_file_path().to_path_buf();
        tracing::info!(
            event = "cache_external_publish_observed",
            cache_path = %cache_path.display(),
            "external publish observed; reloading model strip"
        );
        let loaded = cache::load(&self.paths.cache_root);
        let providers = self.config.providers.value().clone();
        let available = self.available_clis_for_cache_watcher();
        let models =
            crate::data::selection_assembly::assemble_from_loaded(&loaded, &available, &providers);
        if !models.is_empty() {
            self.set_models(models);
            // A fresh on-disk publish satisfies the freshness contract:
            // the follower does NOT need to re-fetch quotas right now,
            // so reset its retry clock the same way a successful refresh
            // would.
            self.model_refresh = ModelRefreshState::Idle(Instant::now());
        }
    }
    fn available_clis_for_cache_watcher(&self) -> BTreeSet<crate::selection::CliKind> {
        crate::data::acp::AcpConfig::from_config_views(
            &self.config.acp.agents,
            &self.config.acp_install_view(),
        )
        .available_clis()
    }
    pub(crate) fn poll_live_summary_mtime(&mut self) {
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
    pub(crate) fn read_live_summary_pipeline(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some((run_model, run_subscription_label)) = self
            .running_run()
            .map(|run| (run.model.clone(), run.subscription_label.clone()))
        else {
            return;
        };
        if !self.active_run_exists(run_id) {
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            return;
        };
        // Cheap mtime probe before reading content: avoids the disk read when
        // the file is missing, stale, or unchanged since the last cached
        // mtime.
        let mtime = match observation::probe_live_summary(&path) {
            LiveSummaryProbe::Fresh { mtime } => mtime,
            LiveSummaryProbe::Missing | LiveSummaryProbe::Stale => return,
        };
        if let Some(cached_mtime) = self.live_summary_cached_mtime
            && mtime <= cached_mtime
        {
            return;
        }
        let Some(LiveSummarySnapshot { content, mtime }) = observation::read_live_summary(&path)
        else {
            return;
        };
        let sanitized = render::sanitize_live_summary(&content);
        // A bare mtime touch with empty or duplicate content does NOT signal
        // operator progress, so it must not reset the watchdog idle clock —
        // an agent that re-flushes the same summary while hung would
        // otherwise slip past the warn threshold. Update cached_mtime
        // either way so we don't re-read on every tick.
        if !live_summary_advances_content(&sanitized, &self.live_summary_cached_text) {
            self.live_summary_cached_mtime = Some(mtime);
            return;
        }
        if let Some(state) = self.watchdog.get_mut(run_id) {
            state.on_live_summary_event(tokio::time::Instant::now());
        }
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: run_model,
                subscription_label: run_subscription_label,
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
    pub(crate) fn tick_watchdog(&mut self) {
        if self.watchdog.is_empty() {
            return;
        }
        let now = tokio::time::Instant::now();
        let decisions = self.watchdog.evaluate_all(now);
        for (run_id, decision) in decisions {
            match decision {
                watchdog::WatchdogDecision::Idle => {}
                watchdog::WatchdogDecision::EmitWarning => {
                    self.dispatch_watchdog_warning(run_id, now);
                }
                watchdog::WatchdogDecision::EmitKill => {
                    self.dispatch_watchdog_kill(run_id, now);
                }
            }
        }
    }
    fn dispatch_watchdog_warning(&mut self, run_id: u64, now: tokio::time::Instant) {
        // Snapshot exactly the values we need before touching dashboard
        // helpers. Holding a borrow on `self.watchdog` across calls to
        // `append_system_message` would deadlock the borrow checker.
        let Some((prompt_path, idle_minutes, remaining_minutes)) =
            self.watchdog.get_mut(run_id).map(|state| {
                (
                    state.prompt_path.clone(),
                    state.idle_minutes_for_message(now),
                    state.warning_remaining_minutes,
                )
            })
        else {
            return;
        };
        let prompt_body = observation::read_prompt_body(&prompt_path)
            .unwrap_or_else(|| watchdog::PROMPT_UNAVAILABLE_BODY.to_string());
        let warning_text = watchdog::warning_text(idle_minutes, remaining_minutes, &prompt_body);
        let interrupt_sent = self
            .runner_supervisor
            .force_interrupt_run(run_id, warning_text);
        let _ = self.state.log_event(format!(
            "watchdog_warning: run={run_id} idle_min={idle_minutes} \
             interrupt_sent={interrupt_sent}"
        ));
        self.append_system_message(
            run_id,
            MessageKind::SummaryWarn,
            format!(
                "watchdog warning: live summary stale for {idle_minutes} min; \
                 sent interrupt with original prompt"
            ),
        );
    }
    fn dispatch_watchdog_kill(&mut self, run_id: u64, now: tokio::time::Instant) {
        let Some(idle_minutes) = self
            .watchdog
            .get_mut(run_id)
            .map(|state| state.idle_minutes_for_message(now))
        else {
            return;
        };
        // Drop the watchdog state immediately. The runner thread
        // observes the Terminate, exits with code 143, and the existing
        // `poll_agent_run` finalize path drives the standard failed-run
        // vendor failover (spec §3.5). `finalize_run_record` will also
        // call remove() but the duplicate is a no-op.
        self.watchdog.remove(run_id);
        let terminate_sent = self.runner_supervisor.terminate_run(run_id);
        let _ = self.state.log_event(format!(
            "watchdog_kill: run={run_id} idle_min={idle_minutes} \
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
    pub(crate) fn drain_live_summary(&mut self, run: &crate::state::RunRecord) {
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
                        subscription_label: run.subscription_label.clone(),
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
