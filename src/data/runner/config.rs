//! Runner-side configuration knobs.
//!
//! Currently houses the periodic-review cadence (`full_review_interval`).
//! Keep this module the single home for runner-level operator knobs.

/// Runner-level config knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerConfig {
    /// Cadence for the full-alignment reviewer prompt. The full-alignment
    /// prompt fires on `ReviewRound(N)` whenever
    /// `full_review_interval > 0 && N > 0 && N % full_review_interval == 0`;
    /// `0` disables the feature so every round uses the regular reviewer.
    /// Default `5` matches the spec — operator-tunable, no CLI flag.
    pub full_review_interval: u32,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            full_review_interval: 5,
        }
    }
}

/// Pick the reviewer prompt for `ReviewRound(round)` given the configured
/// `full_review_interval`. Round 0 (sharding-only) and `interval == 0`
/// always select the regular reviewer; recovery rounds do not increment
/// `round`, so the modulo cadence ignores them automatically.
pub fn select_full_alignment(round: u32, interval: u32) -> bool {
    interval > 0 && round > 0 && round.is_multiple_of(interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_interval_matches_spec() {
        assert_eq!(RunnerConfig::default().full_review_interval, 5);
    }

    #[test]
    fn selection_matrix() {
        // (round, interval, expected_full_alignment)
        let cases = [
            (0, 5, false), // round 0 always regular
            (5, 5, true),  // canonical full-alignment hit
            (3, 5, false), // off-cadence
            (10, 5, true), // every interval
            (1, 0, false), // interval=0 disables
            (5, 0, false), // interval=0 disables even on a multiple
            (0, 0, false), // both zero
            (15, 5, true),
            (4, 4, true),
            (2, 4, false),
        ];
        for (round, interval, expected) in cases {
            assert_eq!(
                select_full_alignment(round, interval),
                expected,
                "round={round} interval={interval}"
            );
        }
    }
}
