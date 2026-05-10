//! Per-run liveness watchdog: pure timing/state machine plus the App-side
//! registry that owns lifecycle wiring.
//!
//! The state machine itself is inert — it does not send interrupts, terminate
//! runs, or write dashboard messages. Higher layers in `lifecycle.rs` /
//! `observation.rs` apply the side effects when `evaluate(now)` returns
//! `EmitWarning` or `EmitKill`.
use crate::adapters::EffortLevel;
use std::{collections::HashMap, path::PathBuf, time::Duration};
use tokio::time::Instant;
/// Identifier used by the watchdog to key per-run state. Mirrors the App's
/// existing `u64` run id (see `state::RunRecord::id`).
pub(super) type RunId = u64;
/// Production thresholds (spec §3.1). The `_TOUGH` variants apply when
/// `EffortLevel == Tough` (1.5×).
pub(crate) const WARN_AFTER_NORMAL: Duration = Duration::from_secs(10 * 60);
pub(crate) const KILL_AFTER_NORMAL: Duration = Duration::from_secs(20 * 60);
pub(crate) const WARN_AFTER_TOUGH: Duration = Duration::from_secs(15 * 60);
pub(crate) const KILL_AFTER_TOUGH: Duration = Duration::from_secs(30 * 60);
/// Idle-adjusted warn threshold for a run with the given effort level
/// (unscaled — production wall-clock duration).
pub(crate) fn warn_after(effort: EffortLevel) -> Duration {
    match effort {
        EffortLevel::Tough => WARN_AFTER_TOUGH,
        EffortLevel::Low | EffortLevel::Normal => WARN_AFTER_NORMAL,
    }
}
/// Idle-adjusted kill threshold for a run with the given effort level
/// (unscaled — production wall-clock duration).
pub(crate) fn kill_after(effort: EffortLevel) -> Duration {
    match effort {
        EffortLevel::Tough => KILL_AFTER_TOUGH,
        EffortLevel::Low | EffortLevel::Normal => KILL_AFTER_NORMAL,
    }
}
/// Decision returned by `WatchdogState::evaluate` once the App has computed a
/// `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchdogDecision {
    Idle,
    EmitWarning,
    EmitKill,
}
/// Per-run wall-clock idle state.
///
/// The watchdog measures plain wall time since the agent last wrote
/// `live_summary.txt`. There's no tool-call exclusion: an agent that
/// disappears down a long tool call without checkpointing the live summary
/// is indistinguishable from one that's hung, and the spec's "every 2–3
/// min" cadence is supposed to keep the summary fresh through tool calls
/// anyway. Anything else that wants to pause the clock should write the
/// live-summary file.
#[derive(Debug, Clone)]
pub(crate) struct WatchdogState {
    pub(super) prompt_path: PathBuf,
    pub(super) last_live_summary_event: Instant,
    pub(super) warned: bool,
    pub(super) warn_threshold: Duration,
    pub(super) kill_threshold: Duration,
    /// Unscaled remaining-minutes value used in the warning preamble. Always
    /// derived from spec constants regardless of clock compression so the
    /// agent-visible text is identical between production and tests.
    pub(super) warning_remaining_minutes: u64,
}
impl WatchdogState {
    /// Construct a state with custom (post-scaled) thresholds. Used by the
    /// registry so it owns the scaling policy in one place. Tests may also
    /// call this to supply hand-picked thresholds.
    pub(crate) fn new_with_thresholds(
        effort: EffortLevel,
        now: Instant,
        prompt_path: PathBuf,
        warn_threshold: Duration,
        kill_threshold: Duration,
    ) -> Self {
        let warn_unscaled = warn_after(effort).as_secs() / 60;
        let kill_unscaled = kill_after(effort).as_secs() / 60;
        let warning_remaining_minutes = kill_unscaled.saturating_sub(warn_unscaled);
        Self {
            prompt_path,
            last_live_summary_event: now,
            warned: false,
            warn_threshold,
            kill_threshold,
            warning_remaining_minutes,
        }
    }
    /// Construct an unscaled state at run-launch time (production
    /// thresholds). Convenience for tests that don't exercise scaling.
    #[cfg(test)]
    pub(crate) fn new(effort: EffortLevel, now: Instant) -> Self {
        Self::new_with_thresholds(
            effort,
            now,
            PathBuf::new(),
            warn_after(effort),
            kill_after(effort),
        )
    }
    /// Wall-clock duration since the last observed `live_summary.txt` mtime
    /// advance — no tool-call exclusion.
    pub(crate) fn idle_elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_live_summary_event)
    }
    /// `live_summary.txt` mtime advance observed at `now`. Resets the idle
    /// clock.
    pub(crate) fn on_live_summary_event(&mut self, now: Instant) {
        self.last_live_summary_event = now;
    }
    /// Decide whether the idle-adjusted clock has crossed a threshold at
    /// `now`. Kill fires even when `warned == false` so a starved App tick
    /// can skip the courtesy warning (spec §3.3 last paragraph).
    pub(crate) fn evaluate(&mut self, now: Instant) -> WatchdogDecision {
        let elapsed = self.idle_elapsed(now);
        if elapsed >= self.kill_threshold {
            return WatchdogDecision::EmitKill;
        }
        if !self.warned && elapsed >= self.warn_threshold {
            self.warned = true;
            return WatchdogDecision::EmitWarning;
        }
        WatchdogDecision::Idle
    }
    /// Idle minutes since the last live-summary mtime advance. Used in the
    /// `SummaryWarn` text and warning preamble.
    pub(crate) fn idle_minutes_for_message(&self, now: Instant) -> u64 {
        self.idle_elapsed(now).as_secs() / 60
    }
}
/// Per-run keyed registry of `WatchdogState`.
#[derive(Debug, Default)]
pub(crate) struct WatchdogRegistry {
    states: HashMap<RunId, WatchdogState>,
}
impl WatchdogRegistry {
    /// Construct an unscaled registry. Used by test scaffolding that does
    /// not exercise clock compression; production callers go through
    /// `from_env` so the env var is honored.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }
    /// Build the production registry. Tests use Tokio's paused clock instead
    /// of compressing thresholds through environment state.
    pub(crate) fn from_env() -> Self {
        Self::default()
    }
    pub(crate) fn warn_threshold(&self, effort: EffortLevel) -> Duration {
        warn_after(effort)
    }
    pub(crate) fn kill_threshold(&self, effort: EffortLevel) -> Duration {
        kill_after(effort)
    }
    /// Insert a fresh watchdog state for a run. Idempotent at the App layer
    /// — callers that re-register without a finalize in between will
    /// overwrite the prior state, which is correct for the resume path
    /// (§4 "State survives across codexize restart? No.").
    pub(crate) fn register(
        &mut self,
        run_id: RunId,
        effort: EffortLevel,
        prompt_path: PathBuf,
        now: Instant,
    ) {
        let warn = self.warn_threshold(effort);
        let kill = self.kill_threshold(effort);
        let state = WatchdogState::new_with_thresholds(effort, now, prompt_path, warn, kill);
        self.states.insert(run_id, state);
    }
    pub(crate) fn remove(&mut self, run_id: RunId) -> Option<WatchdogState> {
        self.states.remove(&run_id)
    }
    pub(crate) fn get_mut(&mut self, run_id: RunId) -> Option<&mut WatchdogState> {
        self.states.get_mut(&run_id)
    }
    #[cfg(test)]
    pub(crate) fn get(&self, run_id: RunId) -> Option<&WatchdogState> {
        self.states.get(&run_id)
    }
    #[cfg(test)]
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut WatchdogState> {
        self.states.values_mut()
    }
    /// Snapshot of (run_id, idle-adjusted decision) pairs at `now`. The
    /// poll-loop calls this and applies side effects per decision while
    /// holding `&mut self`.
    pub(crate) fn evaluate_all(&mut self, now: Instant) -> Vec<(RunId, WatchdogDecision)> {
        self.states
            .iter_mut()
            .map(|(id, state)| (*id, state.evaluate(now)))
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.states.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}
/// Build the warning interrupt payload. The remaining-minutes value comes
/// from the spec constants. `idle_minutes` is the wall-clock minutes since
/// the last live-summary mtime advance, using Tokio's clock so paused-time
/// tests can drive the thresholds deterministically.
pub(crate) fn warning_text(idle_minutes: u64, remaining_minutes: u64, prompt_body: &str) -> String {
    format!(
        "\u{26a0} Liveness warning from codexize watchdog \u{26a0}\n\n\
You have not updated `live_summary.txt` in {idle} minutes. \
You will be killed and the run will be retried automatically if you go another {remaining} minutes without writing a summary. \
This is your only warning.\n\n\
The original prompt is repeated below verbatim so you can resume from it. Continue the task; \
do not acknowledge this warning beyond updating the live summary file.\n\n\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} ORIGINAL PROMPT \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\
{body}\n\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} END ORIGINAL PROMPT \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n",
        idle = idle_minutes,
        remaining = remaining_minutes,
        body = prompt_body,
    )
}
/// Documented degraded fallback when `run.prompt_path` cannot be read —
/// still send a warning rather than silently skipping (spec §3.4).
pub(crate) const PROMPT_UNAVAILABLE_BODY: &str =
    "the original prompt is unavailable on disk; resume the task as best you can.";
#[cfg(test)]
#[path = "watchdog_tests.rs"]
mod tests;
