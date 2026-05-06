use crate::app::test_support::mk_app;
use crate::data::notifications::{
    NotificationEventKind, NotificationReason, NotificationRuntime, NtfyConfig, NtfyDetailMode,
    NtfyPublishPolicy,
};
use crate::state::{
    BlockOrigin, LaunchModes, Message, MessageKind, MessageSender, Phase, RunRecord, RunStatus,
    SessionState,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn state_in_phase(phase: Phase) -> SessionState {
    static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let mut state = SessionState::new(format!("notify-session-{id}"));
    state.current_phase = phase;
    state.title = Some("Readable Session".to_string());
    state
}

fn unique_state_in_phase(phase: Phase, label: &str) -> SessionState {
    let mut state = SessionState::new(format!(
        "notify-{label}-{}",
        chrono::Utc::now().timestamp_nanos_opt().expect("timestamp")
    ));
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

#[test]
fn notification_publish_failures_surface_warning_without_changing_phase() {
    let mut app = app_in_phase(Phase::SpecReviewPaused);
    app.notification_runtime
        .push_publish_failure_for_test("ntfy publish failed after 3 attempts: 503");

    app.poll_notification_reports();

    assert_eq!(app.state.current_phase, Phase::SpecReviewPaused);
    let events_path = crate::state::session_dir(&app.state.session_id).join("events.toml");
    let events = std::fs::read_to_string(events_path).expect("events log");
    assert!(events.contains("notification_publish_failed"));
    assert!(events.contains("503"));
    let status = app
        .status_line
        .borrow()
        .render()
        .expect("warning status")
        .to_string();
    assert!(status.contains("ntfy notification failed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_drain_surfaces_pending_publish_failures_without_changing_phase() {
    let server = FailingNtfyServer::spawn(500).await;
    let mut app = mk_app(unique_state_in_phase(
        Phase::FinalValidation(1),
        "shutdown-drain",
    ));
    app.notification_runtime = NotificationRuntime::from_config_for_test(
        Some(test_ntfy_config(&server.url())),
        NtfyPublishPolicy::for_test(1, Duration::ZERO),
    );

    app.transition_to_phase(Phase::Done)
        .expect("done transition succeeds");
    app.drain_notifications_for_shutdown();

    assert_eq!(app.state.current_phase, Phase::Done);
    let events_path = crate::state::session_dir(&app.state.session_id).join("events.toml");
    let events = std::fs::read_to_string(events_path).expect("events log");
    assert!(events.contains("notification_publish_failed"));
    assert!(events.contains("500"));
    let status = app
        .status_line
        .borrow()
        .render()
        .expect("warning status")
        .to_string();
    assert!(status.contains("ntfy notification failed"));
}

fn test_ntfy_config(server: &str) -> NtfyConfig {
    NtfyConfig {
        version: 1,
        server: server.to_string(),
        topic: "topic-test".to_string(),
        enabled: true,
        detail_mode: NtfyDetailMode::Minimal,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

struct FailingNtfyServer {
    url: String,
}

impl FailingNtfyServer {
    async fn spawn(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_http_request(&mut stream).await;
            let body = "mock";
            let wire = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(wire.as_bytes()).await.expect("write");
        });
        Self { url }
    }

    fn url(&self) -> String {
        self.url.clone()
    }
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) {
    let mut buf = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let n = stream.read(&mut chunk).await.expect("read");
        assert!(n > 0, "request stream closed before headers");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = headers
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let mut chunk = [0_u8; 1024];
        let n = stream.read(&mut chunk).await.expect("read body");
        assert!(n > 0, "request stream closed before body");
        buf.extend_from_slice(&chunk[..n]);
    }
}
