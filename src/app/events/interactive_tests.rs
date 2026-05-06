use crate::app::TerminationIntent;
use crate::app::test_support::mk_app;
use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};

fn running_recovery_run(id: u64) -> RunRecord {
    RunRecord {
        id,
        stage: "recovery".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "kimi-latest".to_string(),
        vendor: "moonshotai".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

#[test]
fn exit_interactive_run_marks_pending_termination_stop_only() {
    // Operator pressing /exit during interactive recovery should mark the run
    // as user-stopped so the post-finalisation failure path skips auto-retry.
    // Without this, an empty/invalid recovery.toml at /exit time triggers
    // `maybe_auto_retry`, silently relaunching the agent — exactly the
    // "recovery with finish does not stop" symptom the operator reported.
    let mut state = SessionState::new("interactive-test".to_string());
    state.current_phase = crate::state::Phase::BuilderRecovery(1);
    state.agent_runs.push(running_recovery_run(7));
    let mut app = mk_app(state);
    app.current_run_id = Some(7);

    app.exit_interactive_run_locally();

    let pending = app
        .pending_termination
        .as_ref()
        .expect("/exit must mark pending termination");
    assert_eq!(pending.run_id, 7);
    assert_eq!(pending.intent, TerminationIntent::StopOnly);
}

#[test]
fn exit_interactive_run_without_active_run_is_a_noop() {
    let state = SessionState::new("interactive-test".to_string());
    let mut app = mk_app(state);

    app.exit_interactive_run_locally();

    assert!(app.pending_termination.is_none());
}

#[test]
fn exit_interactive_run_skips_when_run_not_in_session() {
    // current_run_id points at an id with no matching RunRecord — exit should
    // bail out before queueing pending_termination.
    let state = SessionState::new("interactive-test".to_string());
    let mut app = mk_app(state);
    app.current_run_id = Some(99);

    app.exit_interactive_run_locally();

    assert!(app.pending_termination.is_none());
}
