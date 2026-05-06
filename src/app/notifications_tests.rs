use crate::app::test_support::mk_app;
use crate::data::notifications::{NotificationEventKind, NotificationReason};
use crate::state::{
    BlockOrigin, LaunchModes, Message, MessageKind, MessageSender, Phase, RunRecord, RunStatus,
    SessionState,
};
use std::sync::atomic::{AtomicU64, Ordering};

fn state_in_phase(phase: Phase) -> SessionState {
    static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let mut state = SessionState::new(format!("notify-session-{id}"));
    state.current_phase = phase;
    state.title = Some("Readable Session".to_string());
    state
}

fn app_in_phase(phase: Phase) -> crate::app::App {
    let mut app = mk_app(state_in_phase(phase));
    app.enable_notifications_for_test();
    app
}

fn running_run(id: u64, stage: &str, interactive: bool) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "codex-latest".to_string(),
        vendor: "openai".to_string(),
        window_name: format!("[{stage}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: LaunchModes {
            interactive,
            ..LaunchModes::default()
        },
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

#[test]
fn waiting_phase_transitions_emit_input_needed_events() {
    let cases = [
        (
            Phase::BrainstormRunning,
            Phase::BlockedNeedsUser,
            Some(BlockOrigin::Brainstorm),
            "brainstorm",
        ),
        (
            Phase::SpecReviewRunning,
            Phase::SpecReviewPaused,
            None,
            "spec-review",
        ),
        (
            Phase::PlanReviewRunning,
            Phase::PlanReviewPaused,
            None,
            "plan-review",
        ),
        (
            Phase::BrainstormRunning,
            Phase::SkipToImplPending,
            None,
            "skip-to-impl",
        ),
        (
            Phase::BrainstormRunning,
            Phase::GitGuardPending,
            None,
            "git-guard",
        ),
    ];

    for (from, to, block_origin, expected_stage) in cases {
        let mut app = app_in_phase(from);
        app.state.block_origin = block_origin;

        app.transition_to_phase(to).expect("transition succeeds");

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 1, "{to:?} should emit once");
        assert_eq!(events[0].kind, NotificationEventKind::InputNeeded);
        assert_eq!(events[0].reason, NotificationReason::PhaseWait);
        assert_eq!(events[0].phase, to);
        assert_eq!(events[0].context.stage, expected_stage);
        assert_eq!(events[0].context.session_label, "Readable Session");
    }
}

#[test]
fn done_transition_emits_pipeline_done_event() {
    let mut app = app_in_phase(Phase::FinalValidation(1));

    app.transition_to_phase(Phase::Done)
        .expect("done transition succeeds");

    let events = app.notification_events_for_test();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, NotificationEventKind::PipelineDone);
    assert_eq!(events[0].phase, Phase::Done);
    assert_eq!(events[0].context.stage, "pipeline");
}

#[test]
fn repeated_ticks_in_same_waiting_phase_do_not_emit_duplicates() {
    let mut app = app_in_phase(Phase::SpecReviewRunning);
    app.transition_to_phase(Phase::SpecReviewPaused)
        .expect("pause transition succeeds");

    app.runtime_tick_after_data_drain();
    app.runtime_tick_after_data_drain();

    assert_eq!(app.notification_events_for_test().len(), 1);
}

#[test]
fn re_entering_waiting_phase_emits_a_new_event() {
    let mut app = app_in_phase(Phase::SpecReviewRunning);

    app.transition_to_phase(Phase::SpecReviewPaused)
        .expect("first pause succeeds");
    app.transition_to_phase(Phase::SpecReviewRunning)
        .expect("resume succeeds");
    app.transition_to_phase(Phase::SpecReviewPaused)
        .expect("second pause succeeds");

    let events = app.notification_events_for_test();
    assert_eq!(events.len(), 2);
    assert_ne!(events[0].dedupe_key, events[1].dedupe_key);
}

#[test]
fn same_wait_phase_in_later_stage_is_not_suppressed() {
    let mut app = app_in_phase(Phase::BrainstormRunning);
    app.state.block_origin = Some(BlockOrigin::Brainstorm);

    app.transition_to_phase(Phase::BlockedNeedsUser)
        .expect("brainstorm block succeeds");
    app.transition_to_phase(Phase::PlanningRunning)
        .expect("resume into planning succeeds");
    app.state.block_origin = Some(BlockOrigin::Planning);
    app.transition_to_phase(Phase::BlockedNeedsUser)
        .expect("planning block succeeds");

    let stages: Vec<&str> = app
        .notification_events_for_test()
        .iter()
        .map(|event| event.context.stage.as_str())
        .collect();
    assert_eq!(stages, vec!["brainstorm", "planning"]);
}

#[test]
fn interactive_wait_rising_edge_emits_once_until_next_prompt() {
    let mut state = state_in_phase(Phase::BrainstormRunning);
    state.agent_runs.push(running_run(7, "brainstorm", true));
    let mut app = mk_app(state);
    app.enable_notifications_for_test();
    app.current_run_id = Some(7);
    crate::runner::register_test_run_id("[brainstorm]", 7);
    crate::runner::request_run_label_active_for_test("[brainstorm]");

    app.runtime_tick_after_data_drain();
    assert!(app.notification_events_for_test().is_empty());

    crate::runner::request_run_label_interactive_input_for_test("[brainstorm]");
    app.messages.push(Message {
        ts: chrono::Utc::now(),
        run_id: 7,
        kind: MessageKind::AgentText,
        sender: MessageSender::Agent {
            model: "codex-latest".to_string(),
            vendor: "openai".to_string(),
        },
        text: "Need your input".to_string(),
    });
    app.runtime_tick_after_data_drain();
    app.runtime_tick_after_data_drain();

    let events = app.notification_events_for_test();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, NotificationEventKind::InputNeeded);
    assert_eq!(events[0].reason, NotificationReason::InteractiveRunWait);
    assert_eq!(events[0].context.stage, "brainstorm");
    assert_eq!(events[0].context.run_id, Some(7));
}

#[test]
fn stage_starts_retries_and_mid_run_errors_do_not_emit_events() {
    let mut app = app_in_phase(Phase::IdeaInput);

    app.transition_to_phase(Phase::BrainstormRunning)
        .expect("stage start succeeds");
    app.record_agent_error("mid-run warning");
    app.runtime_tick_after_data_drain();

    assert!(app.notification_events_for_test().is_empty());
}
