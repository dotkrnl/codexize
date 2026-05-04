//! Pure timing/state machine for the per-run liveness watchdog.
//!
//! This module owns the idle-adjusted clock and the warn/kill state machine
//! described in spec §3.1–§3.3. It is intentionally inert at this layer: it
//! does not send interrupts, terminate runs, write dashboard messages, or
//! observe `live_summary.txt` itself. Higher-level App wiring (task 2) is
//! responsible for those side effects.

// Several methods/items here are intentionally consumed only by tests in
// task 1; the policy module that drives warn/kill (task 2) is the rest of
// the consumer surface and lands in a follow-up. The helpers are kept
// pub(super) so task 2 can wire them up without further refactoring.
// Suppress dead-code warnings while the non-test build still has only
// partial consumers.
#![allow(dead_code)]

use crate::adapters::EffortLevel;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Identifier used by the watchdog to key per-run state. Mirrors the App's
/// existing `u64` run id (see `state::RunRecord::id`).
pub(super) type RunId = u64;

pub(super) const WARN_AFTER_NORMAL: Duration = Duration::from_secs(10 * 60);
pub(super) const KILL_AFTER_NORMAL: Duration = Duration::from_secs(20 * 60);
pub(super) const WARN_AFTER_TOUGH: Duration = Duration::from_secs(15 * 60);
pub(super) const KILL_AFTER_TOUGH: Duration = Duration::from_secs(30 * 60);

/// Idle-adjusted warn threshold for a run with the given effort level.
///
/// Goes through this helper (and never the raw constants) so a future
/// test-only clock-compression env var can compose without further
/// refactoring (spec §3.1, §6).
pub(super) fn warn_after(effort: EffortLevel) -> Duration {
    match effort {
        EffortLevel::Tough => WARN_AFTER_TOUGH,
        EffortLevel::Low | EffortLevel::Normal => WARN_AFTER_NORMAL,
    }
}

/// Idle-adjusted kill threshold for a run with the given effort level.
pub(super) fn kill_after(effort: EffortLevel) -> Duration {
    match effort {
        EffortLevel::Tough => KILL_AFTER_TOUGH,
        EffortLevel::Low | EffortLevel::Normal => KILL_AFTER_NORMAL,
    }
}

/// Decision returned by `WatchdogState::evaluate` once the App has computed a
/// `now`. Higher-level App wiring (task 2) maps these onto the actual
/// interrupt/terminate side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WatchdogDecision {
    Idle,
    EmitWarning,
    EmitKill,
}

/// Per-run idle-adjusted timing state. Spec §3.2.
#[derive(Debug, Clone)]
pub(super) struct WatchdogState {
    pub(super) run_id: RunId,
    pub(super) started_at: Instant,
    pub(super) last_live_summary_event: Instant,
    /// Counter (not bool) so concurrent tool calls pause the clock for the
    /// full overlap window without an early unpause when one of several
    /// in-flight calls finishes.
    pub(super) in_flight_tool_calls: usize,
    pub(super) pause_began_at: Option<Instant>,
    /// Pause time accumulated since `last_live_summary_event`. Reset on
    /// every observed live-summary event so future accounting is anchored
    /// to the most recent reset and pre-reset pause windows are not
    /// double-credited.
    pub(super) paused_total: Duration,
    pub(super) warned: bool,
    pub(super) effort: EffortLevel,
}

impl WatchdogState {
    /// Construct a new state at run-launch time. `started_at` and
    /// `last_live_summary_event` are both anchored to `now` so a run that
    /// never produces a first summary still falls under the same warn/kill
    /// timeline measured from launch (spec §4 — first bullet).
    pub(super) fn new(run_id: RunId, effort: EffortLevel, now: Instant) -> Self {
        Self {
            run_id,
            started_at: now,
            last_live_summary_event: now,
            in_flight_tool_calls: 0,
            pause_began_at: None,
            paused_total: Duration::ZERO,
            warned: false,
            effort,
        }
    }

    /// Idle-adjusted duration since the last observed `live_summary.txt`
    /// mtime advance. Subtracts both completed pause windows
    /// (`paused_total`) and any pause window that is still open at `now`.
    /// Spec §3.2.
    pub(super) fn idle_elapsed(&self, now: Instant) -> Duration {
        let raw = now.saturating_duration_since(self.last_live_summary_event);
        let active_pause = self
            .pause_began_at
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default();
        raw.saturating_sub(self.paused_total)
            .saturating_sub(active_pause)
    }

    /// One ACP tool call entered a non-terminal status (`pending` or
    /// `in_progress`). The first such call opens the pause window;
    /// subsequent overlapping calls only bump the counter.
    pub(super) fn on_tool_call_started(&mut self, now: Instant) {
        self.in_flight_tool_calls = self.in_flight_tool_calls.saturating_add(1);
        if self.in_flight_tool_calls == 1 {
            self.pause_began_at = Some(now);
        }
    }

    /// One ACP tool call entered a terminal status. The pause window
    /// closes only when the last in-flight call finishes; the elapsed
    /// pause time is rolled into `paused_total` so future
    /// `idle_elapsed(now)` calls see it.
    ///
    /// Defensive against unbalanced finishes (e.g. a runner that misses
    /// its own Start dedup): if the counter is already zero, this is a
    /// no-op rather than panicking.
    pub(super) fn on_tool_call_finished(&mut self, now: Instant) {
        if self.in_flight_tool_calls == 0 {
            return;
        }
        self.in_flight_tool_calls -= 1;
        if self.in_flight_tool_calls == 0
            && let Some(started) = self.pause_began_at.take()
        {
            self.paused_total = self
                .paused_total
                .saturating_add(now.saturating_duration_since(started));
        }
    }

    /// `live_summary.txt` mtime advance observed at `now`. Resets the
    /// idle clock and the accumulated pause budget. If a tool call is
    /// currently in flight, its pause window is restarted from `now` so
    /// the pre-reset portion of the pause is not double-credited toward
    /// future idle accounting (spec §3.2 corner case).
    pub(super) fn on_live_summary_event(&mut self, now: Instant) {
        self.last_live_summary_event = now;
        self.paused_total = Duration::ZERO;
        if self.pause_began_at.is_some() {
            self.pause_began_at = Some(now);
        }
    }

    /// Decide whether the idle-adjusted clock has crossed a threshold at
    /// `now`. The kill check intentionally fires even when `warned ==
    /// false` so a starved App tick can skip the courtesy warning and go
    /// straight to kill (spec §3.3 last paragraph).
    pub(super) fn evaluate(&mut self, now: Instant) -> WatchdogDecision {
        let elapsed = self.idle_elapsed(now);
        if elapsed >= kill_after(self.effort) {
            return WatchdogDecision::EmitKill;
        }
        if !self.warned && elapsed >= warn_after(self.effort) {
            self.warned = true;
            return WatchdogDecision::EmitWarning;
        }
        WatchdogDecision::Idle
    }
}

/// Runner→App tool-call activity transition. The runner timestamps the
/// transition at the moment it observes the ACP `session/update`; the App
/// applies transitions in arrival (timestamp) order so short calls that
/// start and finish between App polls still count as paused time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolCallTransitionKind {
    Start,
    Finish,
}

/// Per-run keyed registry of `WatchdogState`. Insert-on-launch and
/// remove-on-finalize wiring lives in task 2 (spec §3.8); this struct
/// supplies the storage and the small access helpers task 2 needs.
#[derive(Debug, Default)]
pub(super) struct WatchdogRegistry {
    states: HashMap<RunId, WatchdogState>,
}

impl WatchdogRegistry {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn insert(&mut self, state: WatchdogState) {
        self.states.insert(state.run_id, state);
    }

    pub(super) fn remove(&mut self, run_id: RunId) -> Option<WatchdogState> {
        self.states.remove(&run_id)
    }

    pub(super) fn get_mut(&mut self, run_id: RunId) -> Option<&mut WatchdogState> {
        self.states.get_mut(&run_id)
    }

    #[cfg(test)]
    pub(super) fn get(&self, run_id: RunId) -> Option<&WatchdogState> {
        self.states.get(&run_id)
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut WatchdogState> {
        self.states.values_mut()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.states.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(effort: EffortLevel) -> (Instant, WatchdogState) {
        let now = Instant::now();
        (now, WatchdogState::new(7, effort, now))
    }

    #[test]
    fn idle_elapsed_is_zero_at_launch() {
        let (now, state) = fresh(EffortLevel::Normal);
        assert_eq!(state.idle_elapsed(now), Duration::ZERO);
    }

    #[test]
    fn idle_elapsed_no_tool_calls_tracks_wall_clock() {
        let (start, state) = fresh(EffortLevel::Normal);
        let now = start + Duration::from_secs(7 * 60);
        assert_eq!(state.idle_elapsed(now), Duration::from_secs(7 * 60));
    }

    #[test]
    fn single_tool_call_spans_the_whole_window_no_idle_accumulates() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        // Tool call live for the entire 12-minute window.
        state.on_tool_call_started(start + Duration::from_secs(0));
        let now = start + Duration::from_secs(12 * 60);
        // The pause window is still open; idle_elapsed must subtract it.
        assert_eq!(state.idle_elapsed(now), Duration::ZERO);
    }

    #[test]
    fn overlapping_concurrent_tool_calls_use_counter_not_bool() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        state.on_tool_call_started(start + Duration::from_secs(60));
        state.on_tool_call_started(start + Duration::from_secs(120));
        // First finish must not unpause while the second is still in flight.
        state.on_tool_call_finished(start + Duration::from_secs(180));
        assert!(state.pause_began_at.is_some());
        assert_eq!(state.in_flight_tool_calls, 1);

        // Now finish the second; pause window closes at this instant.
        state.on_tool_call_finished(start + Duration::from_secs(240));
        assert!(state.pause_began_at.is_none());
        // Pause spanned 60..240 (180 s); idle elapsed at t=300 is 300 - 180 = 120.
        let now = start + Duration::from_secs(300);
        assert_eq!(state.idle_elapsed(now), Duration::from_secs(120));
    }

    #[test]
    fn live_summary_event_resets_idle_and_pause_budget() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        state.on_tool_call_started(start + Duration::from_secs(60));
        state.on_tool_call_finished(start + Duration::from_secs(120));
        // 60 s of paused_total before the reset.
        assert_eq!(state.paused_total, Duration::from_secs(60));

        state.on_live_summary_event(start + Duration::from_secs(180));
        assert_eq!(state.paused_total, Duration::ZERO);

        let now = start + Duration::from_secs(300);
        // 120 s since the reset, no pauses since the reset.
        assert_eq!(state.idle_elapsed(now), Duration::from_secs(120));
    }

    #[test]
    fn live_summary_event_during_pause_does_not_double_credit_pre_reset_pause() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        // Tool call starts at t=60 and is still in flight when summary lands.
        state.on_tool_call_started(start + Duration::from_secs(60));
        state.on_live_summary_event(start + Duration::from_secs(120));
        // The pause window must restart from the reset moment, not stay
        // anchored at t=60 (which would over-count pause time by 60 s
        // toward future idle accounting).
        assert_eq!(state.pause_began_at, Some(start + Duration::from_secs(120)));
        assert_eq!(state.paused_total, Duration::ZERO);

        // Tool call finishes at t=300. Pause since reset is 300 - 120 = 180.
        state.on_tool_call_finished(start + Duration::from_secs(300));
        assert_eq!(state.paused_total, Duration::from_secs(180));

        let now = start + Duration::from_secs(420);
        // Idle since reset = 420 - 120 = 300; pause since reset = 180.
        // Idle-adjusted = 300 - 180 = 120.
        assert_eq!(state.idle_elapsed(now), Duration::from_secs(120));
    }

    #[test]
    fn pause_began_at_none_at_start_until_first_tool_call() {
        let (_, state) = fresh(EffortLevel::Normal);
        assert!(state.pause_began_at.is_none());
        assert_eq!(state.in_flight_tool_calls, 0);
    }

    #[test]
    fn unbalanced_finish_is_a_no_op() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        state.on_tool_call_finished(start + Duration::from_secs(10));
        assert_eq!(state.in_flight_tool_calls, 0);
        assert!(state.pause_began_at.is_none());
        assert_eq!(state.paused_total, Duration::ZERO);
    }

    #[test]
    fn warn_threshold_emits_once_then_idle_until_kill() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        // 9 minutes — under warn threshold.
        assert_eq!(
            state.evaluate(start + Duration::from_secs(9 * 60)),
            WatchdogDecision::Idle
        );
        // 11 minutes — first crossing of warn threshold.
        assert_eq!(
            state.evaluate(start + Duration::from_secs(11 * 60)),
            WatchdogDecision::EmitWarning
        );
        assert!(state.warned);
        // 15 minutes — past warn but below kill; one-shot flag suppresses.
        assert_eq!(
            state.evaluate(start + Duration::from_secs(15 * 60)),
            WatchdogDecision::Idle
        );
    }

    #[test]
    fn direct_kill_without_prior_warning_when_starved() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        // App was starved for cycles between warn and kill thresholds; the
        // first evaluation it gets sees idle past the kill threshold.
        let decision = state.evaluate(start + Duration::from_secs(21 * 60));
        assert_eq!(decision, WatchdogDecision::EmitKill);
        // `warned` stays false — the warning was never emitted.
        assert!(!state.warned);
    }

    #[test]
    fn warn_followed_by_kill_when_idle_continues() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(11 * 60)),
            WatchdogDecision::EmitWarning
        );
        assert_eq!(
            state.evaluate(start + Duration::from_secs(21 * 60)),
            WatchdogDecision::EmitKill
        );
    }

    #[test]
    fn tough_thresholds_are_15_30_minutes() {
        assert_eq!(warn_after(EffortLevel::Tough), Duration::from_secs(15 * 60));
        assert_eq!(kill_after(EffortLevel::Tough), Duration::from_secs(30 * 60));
        assert_eq!(
            warn_after(EffortLevel::Normal),
            Duration::from_secs(10 * 60)
        );
        assert_eq!(
            kill_after(EffortLevel::Normal),
            Duration::from_secs(20 * 60)
        );
        assert_eq!(warn_after(EffortLevel::Low), Duration::from_secs(10 * 60));
        assert_eq!(kill_after(EffortLevel::Low), Duration::from_secs(20 * 60));
    }

    #[test]
    fn tough_run_does_not_warn_at_normal_threshold() {
        let (start, mut state) = fresh(EffortLevel::Tough);
        // 11 minutes is past the normal 10-minute warn but below the
        // tough 15-minute warn.
        assert_eq!(
            state.evaluate(start + Duration::from_secs(11 * 60)),
            WatchdogDecision::Idle
        );
        // 16 minutes crosses the tough warn threshold.
        assert_eq!(
            state.evaluate(start + Duration::from_secs(16 * 60)),
            WatchdogDecision::EmitWarning
        );
        // 31 minutes crosses the tough kill threshold.
        assert_eq!(
            state.evaluate(start + Duration::from_secs(31 * 60)),
            WatchdogDecision::EmitKill
        );
    }

    #[test]
    fn registry_insert_get_remove_roundtrip() {
        let mut registry = WatchdogRegistry::new();
        assert!(registry.is_empty());

        let now = Instant::now();
        registry.insert(WatchdogState::new(42, EffortLevel::Tough, now));
        assert_eq!(registry.len(), 1);
        assert!(registry.get(42).is_some());

        if let Some(state) = registry.get_mut(42) {
            state.on_tool_call_started(now);
        }
        assert_eq!(registry.get(42).map(|s| s.in_flight_tool_calls), Some(1));

        let removed = registry.remove(42).expect("was inserted");
        assert_eq!(removed.run_id, 42);
        assert!(registry.is_empty());
        assert!(registry.remove(42).is_none());
    }

    #[test]
    fn registry_iter_mut_visits_all_states() {
        let mut registry = WatchdogRegistry::new();
        let now = Instant::now();
        registry.insert(WatchdogState::new(1, EffortLevel::Normal, now));
        registry.insert(WatchdogState::new(2, EffortLevel::Tough, now));

        for state in registry.iter_mut() {
            state.on_live_summary_event(now + Duration::from_secs(60));
        }
        assert_eq!(
            registry
                .get(1)
                .map(|s| s.last_live_summary_event - now)
                .unwrap_or_default(),
            Duration::from_secs(60)
        );
        assert_eq!(
            registry
                .get(2)
                .map(|s| s.last_live_summary_event - now)
                .unwrap_or_default(),
            Duration::from_secs(60)
        );
    }
}
