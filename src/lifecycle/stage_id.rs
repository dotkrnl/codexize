//! Lifecycle-internal stage identifier.
//!
//! Distinct from [`crate::app_runtime::view::StageId`] (which models only the
//! operator-visible stages used by modals and the keymap) because the slim
//! lifecycle has 14 distinct pipeline stages — Coder/Reviewer/Recovery* and
//! Simplification/RepoStateUpdate all need their own [`Stage`](super::Stage)
//! implementations and registry keys even though the UI groups them under
//! coarser modal categories.
//!
//! Shared by [`super::spec`] and the legacy-run translator so lifecycle code
//! can stay decoupled from the UI's grouped `view::StageId`.
use serde::{Deserialize, Serialize};

/// Identifier for every pipeline stage with its own [`super::Stage`] impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StageId {
    Brainstorm,
    SpecReview,
    Planning,
    PlanReview,
    Sharding,
    Coder,
    Reviewer,
    Recovery,
    RecoveryPlanReview,
    RecoverySharding,
    FinalValidation,
    Simplification,
    Dreaming,
    RepoStateUpdate,
}

impl StageId {
    /// Stable string id used for prompt paths and legacy `RunRecord.stage`
    /// matching.
    pub fn as_str(self) -> &'static str {
        match self {
            StageId::Brainstorm => "brainstorm",
            StageId::SpecReview => "spec-review",
            StageId::Planning => "planning",
            StageId::PlanReview => "plan-review",
            StageId::Sharding => "sharding",
            StageId::Coder => "coder",
            StageId::Reviewer => "reviewer",
            StageId::Recovery => "recovery",
            StageId::RecoveryPlanReview => "recovery-plan-review",
            StageId::RecoverySharding => "recovery-sharding",
            StageId::FinalValidation => "final-validation",
            StageId::Simplification => "simplifier",
            StageId::Dreaming => "dreaming",
            StageId::RepoStateUpdate => "repo-state-update",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_legacy_run_record_literals() {
        // Spot-check the legacy stage strings the current `RunRecord.stage`
        // values use so the V2 cutover can read pre-cutover logs verbatim.
        assert_eq!(StageId::Brainstorm.as_str(), "brainstorm");
        assert_eq!(StageId::Coder.as_str(), "coder");
        assert_eq!(StageId::Simplification.as_str(), "simplifier");
        assert_eq!(StageId::RepoStateUpdate.as_str(), "repo-state-update");
    }
}
