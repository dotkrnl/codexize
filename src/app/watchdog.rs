//! Per-run liveness watchdog: pure timing/state machine plus the App-side
//! registry that owns lifecycle wiring.
//!
//! The state machine itself is inert — it does not send interrupts, terminate
//! runs, or write dashboard messages. Higher layers in `lifecycle.rs` /
//! `observation.rs` apply the side effects when `evaluate(now)` returns
//! `EmitWarning` or `EmitKill`.

use crate::adapters::EffortLevel;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

/// Identifier used by the watchdog to key per-run state. Mirrors the App's
/// existing `u64` run id (see `state::RunRecord::id`).
pub(super) type RunId = u64;

/// Production thresholds (spec §3.1). The `_TOUGH` variants apply when
/// `EffortLevel == Tough` (1.5×).
pub(crate) const WARN_AFTER_NORMAL: Duration = Duration::from_secs(10 * 60);
pub(crate) const KILL_AFTER_NORMAL: Duration = Duration::from_secs(20 * 60);
pub(crate) const WARN_AFTER_TOUGH: Duration = Duration::from_secs(15 * 60);
pub(crate) const KILL_AFTER_TOUGH: Duration = Duration::from_secs(30 * 60);

/// Test-only env var that compresses the watchdog clock (spec §3.1, §6).
/// Production is implicit `1_000_000_000` ns per simulated second (real
/// time). Smaller values shrink real-time thresholds proportionally so the
/// integration tests can drive AC1–AC6 in sub-second wall clock without
/// changing the unscaled spec constants.
pub(crate) const SCALE_ENV_VAR: &str = "CODEXIZE_WATCHDOG_SCALE_NS_PER_SEC";

const PRODUCTION_NS_PER_SEC: u64 = 1_000_000_000;

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
    #[allow(dead_code)] // retained for Debug logs and registry round-trip in tests
    pub(super) run_id: RunId,
    #[allow(dead_code)] // retained for Debug logs and registry round-trip in tests
    pub(super) window_name: String,
    pub(super) prompt_path: PathBuf,
    #[allow(dead_code)] // retained for Debug logs and future operator-visible diagnostics
    pub(super) started_at: Instant,
    pub(super) last_live_summary_event: Instant,
    pub(super) warned: bool,
    pub(super) effort: EffortLevel,
    /// Scaled threshold (real wall-clock duration). Equal to `warn_after`
    /// in production; smaller under clock compression.
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
        run_id: RunId,
        effort: EffortLevel,
        now: Instant,
        window_name: String,
        prompt_path: PathBuf,
        warn_threshold: Duration,
        kill_threshold: Duration,
    ) -> Self {
        let warn_unscaled = warn_after(effort).as_secs() / 60;
        let kill_unscaled = kill_after(effort).as_secs() / 60;
        let warning_remaining_minutes = kill_unscaled.saturating_sub(warn_unscaled);
        Self {
            run_id,
            window_name,
            prompt_path,
            started_at: now,
            last_live_summary_event: now,
            warned: false,
            effort,
            warn_threshold,
            kill_threshold,
            warning_remaining_minutes,
        }
    }

    /// Construct an unscaled state at run-launch time (production
    /// thresholds). Convenience for tests that don't exercise scaling.
    #[cfg(test)]
    pub(crate) fn new(run_id: RunId, effort: EffortLevel, now: Instant) -> Self {
        Self::new_with_thresholds(
            run_id,
            effort,
            now,
            String::new(),
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

    /// Idle minutes since the last live-summary mtime advance, projected
    /// back onto the unscaled (simulated) minute axis. Used in the
    /// `SummaryWarn` text and warning preamble so the agent-visible numbers
    /// match the spec wording independent of clock compression.
    pub(crate) fn idle_minutes_for_message(&self, now: Instant) -> u64 {
        let elapsed_ns = self.idle_elapsed(now).as_nanos();
        let warn_unscaled = warn_after(self.effort).as_nanos();
        let warn_scaled = self.warn_threshold.as_nanos().max(1);
        let simulated_ns = elapsed_ns.saturating_mul(warn_unscaled) / warn_scaled;
        let simulated_secs = (simulated_ns / 1_000_000_000u128) as u64;
        simulated_secs / 60
    }
}

/// Per-run keyed registry of `WatchdogState`. Owns the clock-compression
/// scale once per App so all registered runs share a single policy.
#[derive(Debug)]
pub(crate) struct WatchdogRegistry {
    states: HashMap<RunId, WatchdogState>,
    /// Number of real-time nanoseconds that represent one simulated second.
    /// Production = 1e9; tests may override via `SCALE_ENV_VAR`.
    scale_ns_per_sec: u64,
}

impl Default for WatchdogRegistry {
    fn default() -> Self {
        Self {
            states: HashMap::new(),
            scale_ns_per_sec: PRODUCTION_NS_PER_SEC,
        }
    }
}

impl WatchdogRegistry {
    /// Construct an unscaled registry. Used by test scaffolding that does
    /// not exercise clock compression; production callers go through
    /// `from_env` so the env var is honored.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Build a registry whose threshold scale is read from
    /// `CODEXIZE_WATCHDOG_SCALE_NS_PER_SEC` if present and `> 0`.
    /// Production callers should use this so unset env always means
    /// unscaled (1e9 ns / s) — matches spec §3.1, §6.
    pub(crate) fn from_env() -> Self {
        let scale_ns_per_sec = std::env::var(SCALE_ENV_VAR)
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(PRODUCTION_NS_PER_SEC);
        Self {
            states: HashMap::new(),
            scale_ns_per_sec,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_scale_ns_per_sec(scale_ns_per_sec: u64) -> Self {
        Self {
            states: HashMap::new(),
            scale_ns_per_sec: scale_ns_per_sec.max(1),
        }
    }

    /// Apply the active clock scale to an unscaled `base` duration.
    fn scale(&self, base: Duration) -> Duration {
        if self.scale_ns_per_sec == PRODUCTION_NS_PER_SEC {
            return base;
        }
        // Convert simulated seconds (`base`) into real wall-clock
        // nanoseconds. `u128` keeps the multiplication safe for any
        // realistic spec threshold.
        let secs = u128::from(base.as_secs());
        let sub_nanos = u128::from(base.subsec_nanos());
        let ns_per_sec = u128::from(self.scale_ns_per_sec);
        let scaled_ns = secs.saturating_mul(ns_per_sec)
            + sub_nanos.saturating_mul(ns_per_sec) / 1_000_000_000u128;
        Duration::from_nanos(scaled_ns.min(u128::from(u64::MAX)) as u64)
    }

    pub(crate) fn warn_threshold(&self, effort: EffortLevel) -> Duration {
        self.scale(warn_after(effort))
    }

    pub(crate) fn kill_threshold(&self, effort: EffortLevel) -> Duration {
        self.scale(kill_after(effort))
    }

    /// Insert a fresh watchdog state for a run. Idempotent at the App layer
    /// — callers that re-register without a finalize in between will
    /// overwrite the prior state, which is correct for the resume path
    /// (§4 "State survives across codexize restart? No.").
    pub(crate) fn register(
        &mut self,
        run_id: RunId,
        effort: EffortLevel,
        window_name: String,
        prompt_path: PathBuf,
        now: Instant,
    ) {
        let warn = self.warn_threshold(effort);
        let kill = self.kill_threshold(effort);
        let state = WatchdogState::new_with_thresholds(
            run_id,
            effort,
            now,
            window_name,
            prompt_path,
            warn,
            kill,
        );
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
/// from the unscaled spec constants so the agent-facing text is identical
/// between production and clock-compressed tests. `idle_minutes` is the
/// (unscaled) wall-clock minutes since the last live-summary mtime
/// advance, already mapped onto the spec axis by
/// `WatchdogState::idle_minutes_for_message`.
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
    fn idle_elapsed_tracks_wall_clock() {
        let (start, state) = fresh(EffortLevel::Normal);
        let now = start + Duration::from_secs(7 * 60);
        assert_eq!(state.idle_elapsed(now), Duration::from_secs(7 * 60));
    }

    #[test]
    fn live_summary_event_resets_idle_clock() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        let later = start + Duration::from_secs(8 * 60);
        state.on_live_summary_event(later);
        let now = later + Duration::from_secs(2 * 60);
        assert_eq!(state.idle_elapsed(now), Duration::from_secs(2 * 60));
    }

    #[test]
    fn warn_threshold_emits_once_then_idle_until_kill() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(9 * 60)),
            WatchdogDecision::Idle
        );
        assert_eq!(
            state.evaluate(start + Duration::from_secs(11 * 60)),
            WatchdogDecision::EmitWarning
        );
        assert!(state.warned);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(15 * 60)),
            WatchdogDecision::Idle
        );
    }

    #[test]
    fn direct_kill_without_prior_warning_when_starved() {
        let (start, mut state) = fresh(EffortLevel::Normal);
        let decision = state.evaluate(start + Duration::from_secs(21 * 60));
        assert_eq!(decision, WatchdogDecision::EmitKill);
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
        assert_eq!(
            state.evaluate(start + Duration::from_secs(11 * 60)),
            WatchdogDecision::Idle
        );
        assert_eq!(
            state.evaluate(start + Duration::from_secs(16 * 60)),
            WatchdogDecision::EmitWarning
        );
        assert_eq!(
            state.evaluate(start + Duration::from_secs(31 * 60)),
            WatchdogDecision::EmitKill
        );
    }

    #[test]
    fn registry_register_get_remove_roundtrip() {
        let mut registry = WatchdogRegistry::new();
        assert!(registry.is_empty());

        let now = Instant::now();
        registry.register(
            42,
            EffortLevel::Tough,
            "[Builder t1 r1]".to_string(),
            PathBuf::from("/tmp/prompts/coder.md"),
            now,
        );
        assert_eq!(registry.len(), 1);
        let stored = registry.get(42).expect("registered");
        assert_eq!(stored.window_name, "[Builder t1 r1]");
        assert_eq!(stored.prompt_path.to_str(), Some("/tmp/prompts/coder.md"));
        assert_eq!(stored.warn_threshold, Duration::from_secs(15 * 60));
        assert_eq!(stored.kill_threshold, Duration::from_secs(30 * 60));
        assert_eq!(stored.warning_remaining_minutes, 15);

        let removed = registry.remove(42).expect("was inserted");
        assert_eq!(removed.run_id, 42);
        assert!(registry.is_empty());
        assert!(registry.remove(42).is_none());
    }

    #[test]
    fn registry_iter_mut_visits_all_states() {
        let mut registry = WatchdogRegistry::new();
        let now = Instant::now();
        registry.register(
            1,
            EffortLevel::Normal,
            "[a]".to_string(),
            PathBuf::from("/a"),
            now,
        );
        registry.register(
            2,
            EffortLevel::Tough,
            "[b]".to_string(),
            PathBuf::from("/b"),
            now,
        );

        for state in registry.iter_mut() {
            state.on_live_summary_event(now + Duration::from_secs(60));
        }
        for id in [1, 2] {
            assert_eq!(
                registry
                    .get(id)
                    .map(|s| s.last_live_summary_event - now)
                    .unwrap_or_default(),
                Duration::from_secs(60)
            );
        }
    }

    #[test]
    fn registry_compresses_thresholds_under_test_scale() {
        let mut registry = WatchdogRegistry::with_scale_ns_per_sec(1_000_000);
        let now = Instant::now();
        registry.register(
            1,
            EffortLevel::Normal,
            "[w]".to_string(),
            PathBuf::from("/p"),
            now,
        );
        let state = registry.get(1).expect("registered");
        // 600s simulated × 1_000_000 ns/s = 600_000_000 ns = 600 ms real.
        assert_eq!(state.warn_threshold, Duration::from_millis(600));
        assert_eq!(state.kill_threshold, Duration::from_millis(1200));
        // Warning preamble must still report unscaled minutes.
        assert_eq!(state.warning_remaining_minutes, 10);
    }

    #[test]
    fn registry_evaluate_all_reports_per_run_decisions() {
        let mut registry = WatchdogRegistry::new();
        let now = Instant::now();
        registry.register(
            1,
            EffortLevel::Normal,
            "[a]".to_string(),
            PathBuf::from("/a"),
            now,
        );
        registry.register(
            2,
            EffortLevel::Tough,
            "[b]".to_string(),
            PathBuf::from("/b"),
            now,
        );

        let later = now + Duration::from_secs(11 * 60);
        let mut decisions = registry.evaluate_all(later);
        decisions.sort_by_key(|(id, _)| *id);
        // Run 1 (Normal) crosses warn at 10 min; run 2 (Tough) does not.
        assert_eq!(decisions[0].1, WatchdogDecision::EmitWarning);
        assert_eq!(decisions[1].1, WatchdogDecision::Idle);
    }

    #[test]
    fn warning_text_contains_prompt_verbatim_and_minute_counts() {
        let body = "Original instructions for the agent.";
        let text = warning_text(11, 9, body);
        assert!(text.contains("11 minutes"));
        assert!(text.contains("9 minutes"));
        assert!(text.contains(body));
        assert!(text.contains("ORIGINAL PROMPT"));
        assert!(text.contains("END ORIGINAL PROMPT"));
        // The exact prompt body — including any later lines — is sandwiched
        // between the markers (AC7).
        let start = text.find("ORIGINAL PROMPT").unwrap();
        let end = text.find("END ORIGINAL PROMPT").unwrap();
        assert!(text[start..end].contains(body));
    }

    #[test]
    fn idle_minutes_for_message_reports_unscaled_minutes_under_compression() {
        let mut registry = WatchdogRegistry::with_scale_ns_per_sec(1_000_000);
        let now = Instant::now();
        registry.register(
            1,
            EffortLevel::Normal,
            "[w]".to_string(),
            PathBuf::from("/p"),
            now,
        );
        let state = registry.get_mut(1).expect("registered");
        // 660 ms real wall clock should map back to ~11 simulated minutes.
        let advanced = now + Duration::from_millis(660);
        assert!(state.idle_minutes_for_message(advanced) >= 11);
    }
}
