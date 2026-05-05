//! Typed finalization reason codes.
//!
//! Every variant preserves the exact wire string that the legacy code
//! produced, so `RunRecord.error` (and any test assertions against it)
//! remain compatible. The enum centralises construction so reason-code
//! strings are no longer scattered as literals across the codebase.

/// A finalization reason that may be persisted in `RunRecord.error`.
#[derive(Debug, Clone, PartialEq, Eq, strum::Display, strum::EnumString)]
pub enum Reason {
    #[strum(serialize = "base_missing")]
    BaseMissing,

    #[strum(serialize = "artifact_missing")]
    ArtifactMissing,

    #[strum(serialize = "coder_partial")]
    CoderPartial,

    #[strum(serialize = "missing_coder_summary")]
    MissingCoderSummary,

    #[strum(serialize = "invalid_coder_summary")]
    InvalidCoderSummary,

    #[strum(to_string = "failed_unverified: {detail} at {path}")]
    FailedUnverified { detail: String, path: String },

    #[strum(to_string = "exit({0})")]
    ExitCode(i32),

    #[strum(to_string = "killed({signal_num}) [{detail}]")]
    Killed { signal_num: i32, detail: String },

    #[strum(to_string = "artifact_invalid: {0}")]
    ArtifactInvalid(String),

    #[strum(serialize = "Operator Killed")]
    OperatorKilled,

    #[strum(serialize = "user_forced_retry")]
    UserForcedRetry,

    #[strum(serialize = "forbidden_head_advance")]
    ForbiddenHeadAdvance,

    #[strum(serialize = "reviewer_modified_working_tree")]
    ReviewerModifiedWorkingTree,

    #[strum(to_string = "forbidden_control_edit: {0}")]
    ForbiddenControlEdit(String),

    #[strum(to_string = "recovery_requested_revise: {0}")]
    RecoveryRequestedRevise(String),

    #[strum(to_string = "recovery_requested_human_blocked: {0}")]
    RecoveryRequestedHumanBlocked(String),

    #[strum(to_string = "recovery_requested_agent_pivot: {0}")]
    RecoveryRequestedAgentPivot(String),

    #[strum(to_string = "recovery_plan_review_failed: {0}")]
    RecoveryPlanReviewFailed(String),

    #[strum(to_string = "recovery_sharding_failed: {0}")]
    RecoveryShardingFailed(String),

    #[strum(serialize = "artifact_invalid: recovery summary is empty")]
    RecoverySummaryEmpty,

    #[strum(
        to_string = "artifact_invalid: recovery status={0} requires at least one feedback item"
    )]
    RecoveryMissingFeedback(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_reason_wire_values() {
        assert_eq!(Reason::BaseMissing.to_string(), "base_missing");
        assert_eq!(Reason::ArtifactMissing.to_string(), "artifact_missing");
        assert_eq!(Reason::CoderPartial.to_string(), "coder_partial");
        assert_eq!(
            Reason::MissingCoderSummary.to_string(),
            "missing_coder_summary"
        );
        assert_eq!(
            Reason::InvalidCoderSummary.to_string(),
            "invalid_coder_summary"
        );
        assert_eq!(Reason::OperatorKilled.to_string(), "Operator Killed");
        assert_eq!(Reason::UserForcedRetry.to_string(), "user_forced_retry");
        assert_eq!(
            Reason::ForbiddenHeadAdvance.to_string(),
            "forbidden_head_advance"
        );
        assert_eq!(
            Reason::ReviewerModifiedWorkingTree.to_string(),
            "reviewer_modified_working_tree"
        );
        assert_eq!(
            Reason::RecoverySummaryEmpty.to_string(),
            "artifact_invalid: recovery summary is empty"
        );
    }

    #[test]
    fn static_reason_wire_values_round_trip() {
        assert_eq!(
            "base_missing".parse::<Reason>().unwrap(),
            Reason::BaseMissing
        );
        assert_eq!(
            "artifact_missing".parse::<Reason>().unwrap(),
            Reason::ArtifactMissing
        );
        assert_eq!(
            "missing_coder_summary".parse::<Reason>().unwrap(),
            Reason::MissingCoderSummary
        );
    }

    #[test]
    fn dynamic_reason_wire_values() {
        assert_eq!(Reason::ExitCode(1).to_string(), "exit(1)");
        assert_eq!(
            Reason::Killed {
                signal_num: 15,
                detail: "agent exited 143".to_string(),
            }
            .to_string(),
            "killed(15) [agent exited 143]"
        );
        assert_eq!(
            Reason::FailedUnverified {
                detail: "missing finish stamp".to_string(),
                path: "/tmp/stamp.toml".to_string(),
            }
            .to_string(),
            "failed_unverified: missing finish stamp at /tmp/stamp.toml"
        );
        assert_eq!(
            Reason::ArtifactInvalid("bad toml".to_string()).to_string(),
            "artifact_invalid: bad toml"
        );
        assert_eq!(
            Reason::ForbiddenControlEdit("a.rs, b.rs".to_string()).to_string(),
            "forbidden_control_edit: a.rs, b.rs"
        );
        assert_eq!(
            Reason::RecoveryRequestedRevise("summary".to_string()).to_string(),
            "recovery_requested_revise: summary"
        );
        assert_eq!(
            Reason::RecoveryRequestedHumanBlocked("summary".to_string()).to_string(),
            "recovery_requested_human_blocked: summary"
        );
        assert_eq!(
            Reason::RecoveryRequestedAgentPivot("summary".to_string()).to_string(),
            "recovery_requested_agent_pivot: summary"
        );
        assert_eq!(
            Reason::RecoveryPlanReviewFailed("oops".to_string()).to_string(),
            "recovery_plan_review_failed: oops"
        );
        assert_eq!(
            Reason::RecoveryShardingFailed("oops".to_string()).to_string(),
            "recovery_sharding_failed: oops"
        );
        assert_eq!(
            Reason::RecoveryMissingFeedback("Revise".to_string()).to_string(),
            "artifact_invalid: recovery status=Revise requires at least one feedback item"
        );
    }
}
