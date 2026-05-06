use super::*;
use std::time::SystemTime;
use tempfile::tempdir;

#[test]
fn dispatch_probe_returns_missing_when_path_absent() {
    let dir = tempdir().expect("tempdir");
    let outcome = dispatch(
        DataRequest::ProbeLiveSummary {
            path: dir.path().join("nope.txt"),
        },
        &crate::runner::Supervisor::new(),
    );
    assert_eq!(
        outcome,
        DataOutcome::LiveSummaryProbed(LiveSummaryProbe::Missing)
    );
}

#[test]
fn dispatch_read_returns_none_when_path_absent() {
    let dir = tempdir().expect("tempdir");
    let outcome = dispatch(
        DataRequest::ReadLiveSummary {
            path: dir.path().join("nope.txt"),
        },
        &crate::runner::Supervisor::new(),
    );
    assert_eq!(outcome, DataOutcome::LiveSummaryRead(None));
}

#[test]
fn dispatch_drain_removes_file_after_read() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("live.txt");
    std::fs::write(&path, "draining payload").expect("seed");

    let outcome = dispatch(
        DataRequest::DrainLiveSummary { path: path.clone() },
        &crate::runner::Supervisor::new(),
    );
    match outcome {
        DataOutcome::LiveSummaryDrained(Some(snapshot)) => {
            assert_eq!(snapshot.content, "draining payload");
            // mtime is whatever the OS reported; just assert it's set.
            let _: SystemTime = snapshot.mtime;
        }
        other => panic!("expected drained snapshot, got {other:?}"),
    }
    assert!(!path.exists(), "drain should remove the live-summary file");
}

#[test]
fn dispatch_read_prompt_returns_none_when_missing() {
    let dir = tempdir().expect("tempdir");
    let outcome = dispatch(
        DataRequest::ReadPromptBody {
            path: dir.path().join("missing.prompt"),
        },
        &crate::runner::Supervisor::new(),
    );
    assert_eq!(outcome, DataOutcome::PromptBodyRead(None));
}

#[test]
fn dispatch_interrupt_returns_false_when_no_active_run() {
    let supervisor = crate::runner::Supervisor::new();
    let outcome = dispatch(
        DataRequest::InterruptRun {
            run_id: 999,
            text: "warn".to_string(),
        },
        &supervisor,
    );
    assert_eq!(outcome, DataOutcome::Interrupted(false));
}

#[test]
fn dispatch_terminate_returns_false_when_no_active_run() {
    let supervisor = crate::runner::Supervisor::new();
    let outcome = dispatch(DataRequest::TerminateRun { run_id: 999 }, &supervisor);
    assert_eq!(outcome, DataOutcome::Terminated(false));
}

#[test]
fn dispatch_terminate_routes_through_supervisor() {
    let supervisor = crate::runner::Supervisor::shared_for_test();
    supervisor.shutdown_all_runs();
    let label = "dispatch-active-run";
    crate::runner::register_test_run_id(label, 777);
    crate::runner::request_run_label_active_for_test(label);

    let outcome = dispatch(DataRequest::TerminateRun { run_id: 777 }, &supervisor);

    assert_eq!(outcome, DataOutcome::Terminated(true));
    assert_eq!(
        crate::runner::drain_test_cancel_receiver_for(label),
        vec!["terminate"]
    );
}
