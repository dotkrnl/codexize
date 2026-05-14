//! Slim, round-aware lifecycle [`Phase`].
//!
//! Replaces the 24-variant `Phase` in [`crate::logic::pipeline::phase`] with a
//! compact set of "where in the pipeline are we" values. All `*Paused` and
//! `*Pending` modal states move out of the phase enum into
//! [`super::pending::PendingDecisions`].
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Logical position in the pipeline. Round-aware so the same enum can express
/// "Implementation round 2" or "Review round 3" without separate variants per
/// round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    Idea,
    Spec,
    Plan,
    Implementation(u32),
    Review(u32),
    Finalization,
    Done,
    /// Terminal cancelled state. Intentionally unordered with every other
    /// phase: see [`Phase::partial_cmp`] for the comparison contract.
    Cancelled,
}

impl Phase {
    /// True if this phase has no successor in the linear lifecycle.
    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Done | Phase::Cancelled)
    }

    /// Immediate predecessor used by `:rewind` with no explicit target.
    ///
    /// Round-aware: `Implementation(r)` rewinds to `Review(r-1)` when `r > 1`,
    /// otherwise to `Plan`. `Review(r)` rewinds to `Implementation(r)` (same
    /// round — you cannot finish reviewing what wasn't implemented).
    ///
    /// [`Phase::Idea`] and [`Phase::Cancelled`] return `None`. [`Phase::Done`]
    /// returns `Some(Finalization)` so a rewind from a finished session lands
    /// at the validation pass that produced it.
    pub fn previous(&self) -> Option<Phase> {
        match *self {
            Phase::Idea => None,
            Phase::Spec => Some(Phase::Idea),
            Phase::Plan => Some(Phase::Spec),
            Phase::Implementation(round) => {
                if round <= 1 {
                    Some(Phase::Plan)
                } else {
                    Some(Phase::Review(round - 1))
                }
            }
            Phase::Review(round) => Some(Phase::Implementation(round)),
            Phase::Finalization => {
                // Finalization follows the last completed Review. Without
                // round context we point at the most recent canonical
                // predecessor; callers needing a precise round should
                // explicitly pass a target instead of relying on `previous()`.
                Some(Phase::Review(1))
            }
            Phase::Done => Some(Phase::Finalization),
            Phase::Cancelled => None,
        }
    }

    /// Numeric rank used for the linear ordering. Cancelled has no rank — see
    /// [`Phase::partial_cmp`].
    fn rank(self) -> Option<(u32, u32)> {
        // (major, minor): major is the position along the linear pipeline,
        // minor disambiguates Implementation vs. Review within the same round.
        match self {
            Phase::Idea => Some((0, 0)),
            Phase::Spec => Some((1, 0)),
            Phase::Plan => Some((2, 0)),
            Phase::Implementation(round) => Some((3 + 2 * round, 0)),
            Phase::Review(round) => Some((3 + 2 * round, 1)),
            Phase::Finalization => Some((u32::MAX - 1, 0)),
            Phase::Done => Some((u32::MAX, 0)),
            Phase::Cancelled => None,
        }
    }
}

impl PartialOrd for Phase {
    /// Linear ordering across `Idea < Spec < Plan < Implementation(r) <
    /// Review(r) < Implementation(r+1) < … < Finalization < Done`.
    ///
    /// [`Phase::Cancelled`] is incomparable with every other variant —
    /// `partial_cmp` returns `None` (including `Cancelled` vs. `Cancelled`,
    /// since two cancelled sessions have no meaningful ordering). Callers
    /// that need to treat Cancelled as a terminal sink should use
    /// [`Phase::is_terminal`].
    ///
    /// `Ord` is intentionally not implemented because Cancelled has no
    /// well-defined position in the total order.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self.rank(), other.rank()) {
            (Some(a), Some(b)) => Some(a.cmp(&b)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_linear_across_pipeline() {
        let phases = [
            Phase::Idea,
            Phase::Spec,
            Phase::Plan,
            Phase::Implementation(1),
            Phase::Review(1),
            Phase::Implementation(2),
            Phase::Review(2),
            Phase::Finalization,
            Phase::Done,
        ];
        for window in phases.windows(2) {
            assert!(
                window[0] < window[1],
                "expected {:?} < {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn cancelled_is_incomparable() {
        assert_eq!(Phase::Cancelled.partial_cmp(&Phase::Done), None);
        assert_eq!(Phase::Idea.partial_cmp(&Phase::Cancelled), None);
        assert!(Phase::Cancelled.is_terminal());
        assert!(Phase::Done.is_terminal());
        assert!(!Phase::Idea.is_terminal());
    }

    #[test]
    fn previous_steps_back_one_phase() {
        assert_eq!(Phase::Spec.previous(), Some(Phase::Idea));
        assert_eq!(Phase::Plan.previous(), Some(Phase::Spec));
        assert_eq!(Phase::Implementation(1).previous(), Some(Phase::Plan));
        assert_eq!(Phase::Implementation(2).previous(), Some(Phase::Review(1)));
        assert_eq!(Phase::Review(1).previous(), Some(Phase::Implementation(1)));
        assert_eq!(Phase::Idea.previous(), None);
        assert_eq!(Phase::Done.previous(), Some(Phase::Finalization));
        assert_eq!(Phase::Cancelled.previous(), None);
    }
}
