//! Typed finalization reason codes.
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
