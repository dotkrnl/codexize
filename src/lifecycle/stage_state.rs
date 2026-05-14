//! Slim, round-aware lifecycle [`Stage`].
//!
//! Replaces the 24-variant `Stage` in [`crate::logic::pipeline::stage`] with a
//! compact set of "where in the pipeline are we" values. All `*Paused` and
//! `*Pending` modal states move out of the stage enum into
//! [`super::pending::PendingDecisions`].
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Logical position in the pipeline. Round-aware so the same enum can express
/// "Implementation round 2" or "Review round 3" without separate variants per
/// round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Stage {
    Idea,
    Spec,
    Plan,
    Implementation(u32),
    Review(u32),
    Finalization,
    Done,
    /// Terminal cancelled state. Intentionally unordered with every other
    /// stage: see [`Stage::partial_cmp`] for the comparison contract.
    Cancelled,
}

impl Stage {
    /// True if this stage has no successor in the linear lifecycle.
    pub fn is_terminal(self) -> bool {
        matches!(self, Stage::Done | Stage::Cancelled)
    }

    /// Immediate predecessor used by `:rewind` with no explicit target.
    ///
    /// Round-aware: `Implementation(r)` rewinds to `Review(r-1)` when `r > 1`,
    /// otherwise to `Plan`. `Review(r)` rewinds to `Implementation(r)` (same
    /// round — you cannot finish reviewing what wasn't implemented).
    ///
    /// [`Stage::Idea`] and [`Stage::Cancelled`] return `None`. [`Stage::Done`]
    /// returns `Some(Finalization)` so a rewind from a finished session lands
    /// at the validation pass that produced it.
    pub fn previous(&self) -> Option<Stage> {
        match *self {
            Stage::Idea => None,
            Stage::Spec => Some(Stage::Idea),
            Stage::Plan => Some(Stage::Spec),
            Stage::Implementation(round) => {
                if round <= 1 {
                    Some(Stage::Plan)
                } else {
                    Some(Stage::Review(round - 1))
                }
            }
            Stage::Review(round) => Some(Stage::Implementation(round)),
            Stage::Finalization => {
                // Finalization follows the last completed Review. Without
                // round context we point at the most recent canonical
                // predecessor; callers needing a precise round should
                // explicitly pass a target instead of relying on `previous()`.
                Some(Stage::Review(1))
            }
            Stage::Done => Some(Stage::Finalization),
            Stage::Cancelled => None,
        }
    }

    /// Numeric rank used for the linear ordering. Cancelled has no rank — see
    /// [`Stage::partial_cmp`].
    fn rank(self) -> Option<(u32, u32)> {
        // (major, minor): major is the position along the linear pipeline,
        // minor disambiguates Implementation vs. Review within the same round.
        match self {
            Stage::Idea => Some((0, 0)),
            Stage::Spec => Some((1, 0)),
            Stage::Plan => Some((2, 0)),
            Stage::Implementation(round) => Some((3 + 2 * round, 0)),
            Stage::Review(round) => Some((3 + 2 * round, 1)),
            Stage::Finalization => Some((u32::MAX - 1, 0)),
            Stage::Done => Some((u32::MAX, 0)),
            Stage::Cancelled => None,
        }
    }
}

impl PartialOrd for Stage {
    /// Linear ordering across `Idea < Spec < Plan < Implementation(r) <
    /// Review(r) < Implementation(r+1) < … < Finalization < Done`.
    ///
    /// [`Stage::Cancelled`] is incomparable with every other variant —
    /// `partial_cmp` returns `None` (including `Cancelled` vs. `Cancelled`,
    /// since two cancelled sessions have no meaningful ordering). Callers
    /// that need to treat Cancelled as a terminal sink should use
    /// [`Stage::is_terminal`].
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
        let stages = [
            Stage::Idea,
            Stage::Spec,
            Stage::Plan,
            Stage::Implementation(1),
            Stage::Review(1),
            Stage::Implementation(2),
            Stage::Review(2),
            Stage::Finalization,
            Stage::Done,
        ];
        for window in stages.windows(2) {
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
        assert_eq!(Stage::Cancelled.partial_cmp(&Stage::Done), None);
        assert_eq!(Stage::Idea.partial_cmp(&Stage::Cancelled), None);
        assert!(Stage::Cancelled.is_terminal());
        assert!(Stage::Done.is_terminal());
        assert!(!Stage::Idea.is_terminal());
    }

    #[test]
    fn previous_steps_back_one_stage() {
        assert_eq!(Stage::Spec.previous(), Some(Stage::Idea));
        assert_eq!(Stage::Plan.previous(), Some(Stage::Spec));
        assert_eq!(Stage::Implementation(1).previous(), Some(Stage::Plan));
        assert_eq!(Stage::Implementation(2).previous(), Some(Stage::Review(1)));
        assert_eq!(Stage::Review(1).previous(), Some(Stage::Implementation(1)));
        assert_eq!(Stage::Idea.previous(), None);
        assert_eq!(Stage::Done.previous(), Some(Stage::Finalization));
        assert_eq!(Stage::Cancelled.previous(), None);
    }
}
