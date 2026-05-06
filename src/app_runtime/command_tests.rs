use super::*;

#[test]
fn commands_are_owned_and_hashable_friendly() {
    // The seam only requires Clone + PartialEq + Eq + Debug, which is
    // exercised by the derive. This test pins the value-type contract
    // so future variants cannot accidentally introduce non-clone
    // payloads.
    let cmd = AppCommand::SubmitInput {
        text: "hello".to_string(),
    };
    let cloned = cmd.clone();
    assert_eq!(cmd, cloned);
}

#[test]
fn retry_stage_carries_stage_identifier() {
    let cmd = AppCommand::RetryStage(StageId::Planning);
    match cmd {
        AppCommand::RetryStage(StageId::Planning) => {}
        other => panic!("expected RetryStage(Planning), got {other:?}"),
    }
}
