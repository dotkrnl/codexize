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

#[cfg(test)]
mod tests {
    use super::Reason;

    fn samples() -> Vec<(&'static str, String)> {
        vec![
            ("BaseMissing", Reason::BaseMissing.to_string()),
            ("ArtifactMissing", Reason::ArtifactMissing.to_string()),
            ("CoderPartial", Reason::CoderPartial.to_string()),
            (
                "MissingCoderSummary",
                Reason::MissingCoderSummary.to_string(),
            ),
            (
                "InvalidCoderSummary",
                Reason::InvalidCoderSummary.to_string(),
            ),
            (
                "FailedUnverified",
                Reason::FailedUnverified {
                    detail: "missing finish stamp".into(),
                    path: "/tmp/stamp.toml".into(),
                }
                .to_string(),
            ),
            ("ExitCode", Reason::ExitCode(1).to_string()),
            (
                "Killed",
                Reason::Killed {
                    signal_num: 15,
                    detail: "agent exited 143".into(),
                }
                .to_string(),
            ),
            (
                "ArtifactInvalid",
                Reason::ArtifactInvalid("bad toml".into()).to_string(),
            ),
            ("OperatorKilled", Reason::OperatorKilled.to_string()),
            ("UserForcedRetry", Reason::UserForcedRetry.to_string()),
            (
                "ForbiddenHeadAdvance",
                Reason::ForbiddenHeadAdvance.to_string(),
            ),
            (
                "ReviewerModifiedWorkingTree",
                Reason::ReviewerModifiedWorkingTree.to_string(),
            ),
            (
                "ForbiddenControlEdit",
                Reason::ForbiddenControlEdit("a.rs, b.rs".into()).to_string(),
            ),
            (
                "RecoveryRequestedRevise",
                Reason::RecoveryRequestedRevise("summary".into()).to_string(),
            ),
            (
                "RecoveryRequestedHumanBlocked",
                Reason::RecoveryRequestedHumanBlocked("summary".into()).to_string(),
            ),
            (
                "RecoveryRequestedAgentPivot",
                Reason::RecoveryRequestedAgentPivot("summary".into()).to_string(),
            ),
            (
                "RecoveryPlanReviewFailed",
                Reason::RecoveryPlanReviewFailed("oops".into()).to_string(),
            ),
            (
                "RecoveryShardingFailed",
                Reason::RecoveryShardingFailed("oops".into()).to_string(),
            ),
            (
                "RecoverySummaryEmpty",
                Reason::RecoverySummaryEmpty.to_string(),
            ),
            (
                "RecoveryMissingFeedback",
                Reason::RecoveryMissingFeedback("Revise".into()).to_string(),
            ),
        ]
    }

    #[test]
    fn static_reason_wire_values_round_trip() {
        for reason in [
            Reason::BaseMissing,
            Reason::ArtifactMissing,
            Reason::MissingCoderSummary,
        ] {
            assert_eq!(reason.to_string().parse::<Reason>().unwrap(), reason);
        }
    }

    #[test]
    fn reason_matrix_snapshot() {
        insta::assert_snapshot!(
            "finalization_reason_matrix",
            samples()
                .into_iter()
                .map(|(variant, wire)| format!("{variant} => {wire}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
