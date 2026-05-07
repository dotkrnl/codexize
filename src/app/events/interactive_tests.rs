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
        route_provider: None,
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

fn running_noninteractive_run(id: u64) -> RunRecord {
    let mut run = running_recovery_run(id);
    run.modes.interactive = false;
    run
}

fn running_interactive_run(id: u64) -> RunRecord {
    let mut run = running_recovery_run(id);
    run.modes.interactive = true;
    run
}

fn finished_run(id: u64) -> RunRecord {
    let mut run = running_recovery_run(id);
    run.status = RunStatus::Done;
    run
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

#[test]
fn palette_interrupt_registered_for_noninteractive_running_agent() {
    let mut state = SessionState::new("interrupt-palette-test".to_string());
    state.agent_runs.push(running_noninteractive_run(10));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must appear in palette when a non-interactive agent is running"
    );
}

#[test]
fn palette_interrupt_registered_for_interactive_running_agent() {
    let mut state = SessionState::new("interrupt-palette-test".to_string());
    state.agent_runs.push(running_interactive_run(11));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must appear in palette when an interactive agent is running"
    );
}

#[test]
fn palette_interrupt_absent_when_no_agent_running() {
    let mut state = SessionState::new("interrupt-palette-test".to_string());
    state.agent_runs.push(finished_run(12));
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        !commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must not appear in palette when no agent is running"
    );
}

#[test]
fn palette_interrupt_absent_with_empty_runs() {
    let state = SessionState::new("interrupt-palette-test".to_string());
    let app = mk_app(state);
    let commands = app.palette_commands();
    assert!(
        !commands.iter().any(|cmd| cmd.name == "interrupt"),
        ":interrupt must not appear in palette when there are no runs"
    );
}
