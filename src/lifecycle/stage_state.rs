//! Compact, round-aware lifecycle [`Stage`].
//!
//! The canonical pipeline position type. All `*Paused` and `*Pending` modal
//! states live in [`super::pending::PendingDecisions`], not here.
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Logical position in the pipeline. Round-aware so the same enum can express
/// "Implementation round 2" or "Review round 3" without separate variants per
/// round.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Stage {
    #[default]
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
    /// Operator-blocked state. Ordered like Cancelled (incomparable). The
    /// session stays at whatever pipeline position caused the block; the
    /// block reason lives in [`crate::state::SessionState::agent_error`].
    Blocked,
    IdeaInput,
    BrainstormRunning,
    SpecReviewRunning,
    SpecReviewPaused,
    PlanningRunning,
    PlanReviewRunning,
    PlanReviewPaused,
    WaitingToImplement,
    RepoStateUpdateRunning,
    ShardingRunning,
    SkipToImplPending,
    BuilderRecovery(u32),
    BuilderRecoveryPlanReview(u32),
    BuilderRecoverySharding(u32),
    GitGuardPending,
    FinalValidation(u32),
    DreamingPending,
    Dreaming(u32),
    Simplification(u32),
    BlockedNeedsUser,
}

impl Stage {
    pub fn to_lifecycle_stage(self) -> Stage {
        match self {
            Stage::Idea | Stage::IdeaInput | Stage::BrainstormRunning => Stage::Idea,
            Stage::Spec | Stage::SpecReviewRunning | Stage::SpecReviewPaused => Stage::Spec,
            Stage::Plan
            | Stage::PlanningRunning
            | Stage::PlanReviewRunning
            | Stage::PlanReviewPaused
            | Stage::WaitingToImplement
            | Stage::RepoStateUpdateRunning
            | Stage::ShardingRunning
            | Stage::SkipToImplPending
            | Stage::GitGuardPending => Stage::Plan,
            Stage::Implementation(r)
            | Stage::BuilderRecovery(r)
            | Stage::BuilderRecoveryPlanReview(r)
            | Stage::BuilderRecoverySharding(r) => Stage::Implementation(r),
            Stage::Review(r) | Stage::Simplification(r) => Stage::Review(r),
            Stage::Finalization
            | Stage::FinalValidation(_)
            | Stage::DreamingPending
            | Stage::Dreaming(_) => Stage::Finalization,
            Stage::Done => Stage::Done,
            Stage::Cancelled => Stage::Cancelled,
            Stage::Blocked | Stage::BlockedNeedsUser => Stage::Blocked,
        }
    }

    pub fn from_lifecycle_stage(stage: Stage) -> Stage {
        match stage {
            Stage::Idea => Stage::IdeaInput,
            Stage::Spec => Stage::SpecReviewRunning,
            Stage::Plan => Stage::PlanningRunning,
            other => other,
        }
    }

    /// True if this stage has no successor in the linear lifecycle.
    pub fn is_terminal(self) -> bool {
        matches!(self, Stage::Done | Stage::Cancelled)
    }

    pub fn is_blocked(self) -> bool {
        matches!(self, Stage::Blocked | Stage::BlockedNeedsUser)
    }

    pub fn round(self) -> Option<u32> {
        match self {
            Stage::Implementation(r)
            | Stage::Review(r)
            | Stage::BuilderRecovery(r)
            | Stage::BuilderRecoveryPlanReview(r)
            | Stage::BuilderRecoverySharding(r)
            | Stage::FinalValidation(r)
            | Stage::Dreaming(r)
            | Stage::Simplification(r) => Some(r),
            _ => None,
        }
    }

    pub fn label(self) -> String {
        let label = match self {
            Stage::Idea => "Idea Input",
            Stage::Spec => "Spec Review",
            Stage::Plan => "Planning",
            Stage::Implementation(_) => "Implementation",
            Stage::Review(_) => "Review",
            Stage::Finalization => "Finalization",
            Stage::Done => "Done",
            Stage::Cancelled => "Cancelled",
            Stage::Blocked => "Blocked",
            Stage::IdeaInput => "Idea Input",
            Stage::BrainstormRunning => "Brainstorming",
            Stage::SpecReviewRunning | Stage::SpecReviewPaused => "Spec Review",
            Stage::PlanningRunning => "Planning",
            Stage::PlanReviewRunning | Stage::PlanReviewPaused => "Plan Review",
            Stage::WaitingToImplement => "Waiting to implement",
            Stage::RepoStateUpdateRunning => "Updating plan",
            Stage::ShardingRunning => "Sharding",
            Stage::SkipToImplPending => "Skip Confirmation",
            Stage::BuilderRecovery(_) => "Builder Recovery",
            Stage::BuilderRecoveryPlanReview(_) => "Recovery Plan Review",
            Stage::BuilderRecoverySharding(_) => "Recovery Sharding",
            Stage::GitGuardPending => "Guard Decision",
            Stage::FinalValidation(_) => "Final Validation",
            Stage::DreamingPending | Stage::Dreaming(_) => "Dreaming",
            Stage::Simplification(_) => "Simplification",
            Stage::BlockedNeedsUser => "Blocked",
        };
        label.to_string()
    }

    pub fn display_label(self) -> String {
        match self {
            Stage::Implementation(r) => format!("Implementation Round {r}"),
            Stage::Review(r) => format!("Review Round {r}"),
            Stage::FinalValidation(r) => format!("Final Validation Round {r}"),
            Stage::Dreaming(r) => format!("Dreaming Round {r}"),
            Stage::Simplification(r) => format!("Simplification Round {r}"),
            other => other.label(),
        }
    }

    pub fn is_running(self) -> bool {
        matches!(
            self,
            Stage::Idea
                | Stage::Spec
                | Stage::Plan
                | Stage::Implementation(_)
                | Stage::Review(_)
                | Stage::Finalization
                | Stage::BrainstormRunning
                | Stage::SpecReviewRunning
                | Stage::PlanningRunning
                | Stage::PlanReviewRunning
                | Stage::RepoStateUpdateRunning
                | Stage::ShardingRunning
                | Stage::BuilderRecovery(_)
                | Stage::BuilderRecoveryPlanReview(_)
                | Stage::BuilderRecoverySharding(_)
                | Stage::FinalValidation(_)
                | Stage::Dreaming(_)
                | Stage::Simplification(_)
        )
    }

    pub fn is_waiting(self) -> bool {
        matches!(
            self,
            Stage::Blocked
                | Stage::BlockedNeedsUser
                | Stage::Cancelled
                | Stage::SpecReviewPaused
                | Stage::PlanReviewPaused
                | Stage::WaitingToImplement
                | Stage::SkipToImplPending
                | Stage::GitGuardPending
                | Stage::DreamingPending
        )
    }

    pub fn stage_lane(self) -> StageLane {
        match self {
            Stage::Idea
            | Stage::Spec
            | Stage::Plan
            | Stage::BrainstormRunning
            | Stage::SpecReviewRunning
            | Stage::SpecReviewPaused
            | Stage::PlanningRunning
            | Stage::PlanReviewRunning
            | Stage::PlanReviewPaused => StageLane::Planning,
            Stage::Implementation(_)
            | Stage::Review(_)
            | Stage::Finalization
            | Stage::RepoStateUpdateRunning
            | Stage::ShardingRunning
            | Stage::BuilderRecovery(_)
            | Stage::BuilderRecoveryPlanReview(_)
            | Stage::BuilderRecoverySharding(_)
            | Stage::FinalValidation(_)
            | Stage::Dreaming(_)
            | Stage::Simplification(_) => StageLane::Implementation,
            Stage::Done
            | Stage::Blocked
            | Stage::BlockedNeedsUser
            | Stage::Cancelled
            | Stage::IdeaInput
            | Stage::WaitingToImplement
            | Stage::SkipToImplPending
            | Stage::GitGuardPending => StageLane::Other,
            Stage::DreamingPending => StageLane::Implementation,
        }
    }

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
            Stage::Finalization => Some(Stage::Review(1)),
            Stage::Done => Some(Stage::Finalization),
            Stage::Cancelled => None,
            Stage::Blocked => None,
            Stage::IdeaInput => None,
            other => other.to_lifecycle_stage().previous(),
        }
    }

    fn rank(self) -> Option<(u32, u32)> {
        match self.to_lifecycle_stage() {
            Stage::Idea => Some((0, 0)),
            Stage::Spec => Some((1, 0)),
            Stage::Plan => Some((2, 0)),
            Stage::Implementation(round) => Some((3 + 2 * round, 0)),
            Stage::Review(round) => Some((3 + 2 * round, 1)),
            Stage::Finalization => Some((u32::MAX - 1, 0)),
            Stage::Done => Some((u32::MAX, 0)),
            Stage::Cancelled | Stage::Blocked => None,
            _ => unreachable!("to_lifecycle_stage returns only compact stages"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageLane {
    Planning,
    Implementation,
    Other,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stage::Implementation(r) => write!(f, "Implementation Round {r}"),
            Stage::Review(r) => write!(f, "Review Round {r}"),
            Stage::FinalValidation(r) => write!(f, "Final Validation Round {r}"),
            Stage::Dreaming(r) => write!(f, "Dreaming Round {r}"),
            Stage::Simplification(r) => write!(f, "Simplification Round {r}"),
            _ => write!(f, "{}", self.label()),
        }
    }
}

impl PartialOrd for Stage {
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
        assert!(Stage::Blocked.is_blocked());
        assert!(Stage::Blocked.is_waiting());
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
        assert_eq!(Stage::Blocked.previous(), None);
    }
}
