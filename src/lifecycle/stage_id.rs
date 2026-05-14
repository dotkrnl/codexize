//! Lifecycle-internal stage identifier.
//!
//! Distinct from [`crate::app_runtime::view::StageId`] (which models only the
//! operator-visible stages used by modals and the keymap) because the slim
//! lifecycle has 14 distinct pipeline stages — Coder/Reviewer/Recovery* and
//! Simplification/RepoStateUpdate all need their own [`Stage`](super::Stage)
//! implementations and registry keys even though the UI groups them under
//! coarser modal categories.
//!
//! Shared by [`super::spec`] and the persisted-run translator so lifecycle code
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
    /// Stable string id used for prompt paths and persisted `RunRecord.stage`
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

/// Best-effort lifecycle [`StageId`] for a persisted run record's `stage` string
/// and `window_name` discriminators.
///
/// Synthesizes a [`StageId`] from the existing `RunRecord` fields.
/// Recovery sub-stages share the `stage == "recovery"` string, so we key off
/// the human-readable window label to preserve fidelity.
pub fn stage_id_for_run(stage: &str, window_name: &str) -> Option<StageId> {
    if window_name.contains("[Recovery Plan Review]") {
        return Some(StageId::RecoveryPlanReview);
    }
    if window_name.contains("[Recovery Sharding]") {
        return Some(StageId::RecoverySharding);
    }
    Some(match stage {
        "brainstorm" => StageId::Brainstorm,
        "spec-review" => StageId::SpecReview,
        "planning" => StageId::Planning,
        "plan-review" => StageId::PlanReview,
        "sharding" => StageId::Sharding,
        "recovery" => StageId::Recovery,
        "coder" => StageId::Coder,
        "reviewer" => StageId::Reviewer,
        "final-validation" => StageId::FinalValidation,
        "simplifier" => StageId::Simplification,
        "dreaming" => StageId::Dreaming,
        "repo-state-update" => StageId::RepoStateUpdate,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_persisted_run_record_literals() {
        // Spot-check the persisted stage strings the current `RunRecord.stage`
        // values use so the V2 cutover can read pre-cutover logs verbatim.
        assert_eq!(StageId::Brainstorm.as_str(), "brainstorm");
        assert_eq!(StageId::Coder.as_str(), "coder");
        assert_eq!(StageId::Simplification.as_str(), "simplifier");
        assert_eq!(StageId::RepoStateUpdate.as_str(), "repo-state-update");
    }

    #[test]
    fn stage_id_for_run_handles_recovery_subwindows() {
        assert_eq!(
            stage_id_for_run("recovery", "[Recovery Plan Review]"),
            Some(StageId::RecoveryPlanReview)
        );
        assert_eq!(
            stage_id_for_run("recovery", "[Recovery Sharding] r1"),
            Some(StageId::RecoverySharding)
        );
        assert_eq!(
            stage_id_for_run("recovery", "[Recovery]"),
            Some(StageId::Recovery)
        );
    }

    #[test]
    fn stage_id_for_run_maps_stage_strings() {
        assert_eq!(
            stage_id_for_run("coder", "[Builder r1]"),
            Some(StageId::Coder)
        );
        assert_eq!(
            stage_id_for_run("simplifier", "[Simplifier]"),
            Some(StageId::Simplification)
        );
        assert_eq!(stage_id_for_run("unknown-stage", ""), None);
    }
}
