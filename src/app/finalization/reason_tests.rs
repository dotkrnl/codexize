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
